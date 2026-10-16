import { describe, expect, test } from "vitest";
import { ApiEnroller } from "../src/experiments.ts";
import { site } from "./fixtures.ts";

const EXP = { id: "11111111-1111-4111-8111-111111111111", version: 3 };
const ALICE = "a".repeat(32);

describe("ApiEnroller", () => {
  test("asks the API once per subject and experiment version for 60 s", async () => {
    const seen: string[] = [];
    let arm = "a";
    const enroller = new ApiEnroller("http://api.test", "service-token", async (r) => {
      seen.push(`${r.method} ${r.url} ${r.headers.get("authorization")} ${await r.text()}`);
      return Response.json({ assignment: { experiment_id: EXP.id, version: 3, arm } });
    });
    const s = site({ experiment: EXP });
    const want = { experiment_id: EXP.id, version: 3, arm: "a" };
    expect(await enroller.enrol(s, ALICE, 0)).toEqual(want);
    arm = "b";
    expect(await enroller.enrol(s, ALICE, 59_999)).toEqual(want);
    expect(seen).toEqual([
      `POST http://api.test/internal/v1/experiments/enrol Bearer service-token {"tenant_id":"t-demo","subject":"${ALICE}"}`,
    ]);
    expect(await enroller.enrol(site({ experiment: { ...EXP, version: 4 } }), ALICE, 1)).toEqual({
      ...want,
      arm: "b",
    });
    expect(await enroller.enrol(s, ALICE, 60_001)).toEqual({ ...want, arm: "b" });
    enroller.forget("t-demo", ALICE);
    expect(await enroller.enrol(s, ALICE, 60_002)).toEqual({ ...want, arm: "b" });
    expect(seen.length).toBe(4);
  });

  test("failures throw and are retried; malformed answers count as not enrolled", async () => {
    const answers: (() => Response)[] = [
      () => new Response("boom", { status: 500 }),
      () => Response.json({ assignment: { experiment_id: EXP.id, version: 3, arm: "A B" } }),
      () => Response.json({ assignment: { experiment_id: EXP.id, version: 3.5, arm: "a" } }),
      () => Response.json({ assignment: null }),
    ];
    let calls = 0;
    const enroller = new ApiEnroller("http://api.test", "t", async () => {
      const answer = answers[calls++];
      if (!answer) throw new Error("unexpected call");
      return answer();
    });
    const s = site({ experiment: EXP });
    await expect(enroller.enrol(s, ALICE, 0)).rejects.toThrow("enrol: 500");
    expect(await enroller.enrol(s, ALICE, 0)).toBeNull();
    enroller.forget("t-demo", ALICE);
    expect(await enroller.enrol(s, ALICE, 0)).toBeNull();
    enroller.forget("t-demo", ALICE);
    expect(await enroller.enrol(s, ALICE, 0)).toBeNull();
    expect(calls).toBe(4);
  });

  test("gives up after 500 ms", async () => {
    const enroller = new ApiEnroller(
      "http://api.test",
      "t",
      (r) => new Promise((_, reject) => r.signal.addEventListener("abort", () => reject(r.signal.reason))),
    );
    const started = Date.now();
    await expect(enroller.enrol(site({ experiment: EXP }), ALICE)).rejects.toThrow();
    expect(Date.now() - started).toBeLessThan(2_000);
  });
});
