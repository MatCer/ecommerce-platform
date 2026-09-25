import { describe, expect, it } from "vitest";
import { siblingOrigin } from "./config.ts";
import { isFresh, readClaims } from "./jwt.ts";

function token(payload: object): string {
  const enc = (o: object) =>
    btoa(String.fromCharCode(...new TextEncoder().encode(JSON.stringify(o))))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
  return `${enc({ alg: "EdDSA" })}.${enc(payload)}.sig`;
}

describe("readClaims", () => {
  it("reads the staff claims, including non-ASCII emails", () => {
    const c = readClaims(token({ sub: "u1", email: "žofie@example.cz", exp: 100, auth_time: 40 }));
    expect(c).toEqual({ sub: "u1", email: "žofie@example.cz", exp: 100, auth_time: 40 });
  });

  it("rejects malformed tokens", () => {
    expect(readClaims("nope")).toBeNull();
    expect(readClaims("a.b.c")).toBeNull();
    expect(readClaims(token({ email: "x" }))).toBeNull();
  });

  it("refreshes a minute before expiry", () => {
    const c = { sub: "u", email: "", exp: 1000, auth_time: 0 };
    expect(isFresh(c, 900)).toBe(true);
    expect(isFresh(c, 940)).toBe(false);
  });
});

describe("siblingOrigin", () => {
  it("swaps the admin subdomain and keeps scheme and port", () => {
    expect(siblingOrigin("http://admin.localhost:8580", "api")).toBe("http://api.localhost:8580");
    expect(siblingOrigin("https://admin.example.com", "s3")).toBe("https://s3.example.com");
  });
});
