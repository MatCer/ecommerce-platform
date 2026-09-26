#!/usr/bin/env node
/**
 * Real password-manager autofill check for the checkout address form (WP26).
 *
 * Starts a throwaway Vaultwarden (docker, self-signed TLS), registers a user with a Slovak
 * Identity item, loads the released Bitwarden Chromium extension into Playwright, fills the
 * checkout through the extension's inline menu and prints every field value. It then runs
 * Chromium's own address autofill (CDP Autofill.trigger) on the same form.
 *
 *   xvfb-run -a node scripts/autofill-check.mjs            # billing address only
 *   SHIP=1 xvfb-run -a node scripts/autofill-check.mjs     # with a separate delivery address
 *
 * Needs the local stack (make up / make theme-build), docker, gh, openssl, xvfb-run.
 * Exits non-zero when a field is wrong. Removes the container and temp dir at the end.
 */
import { execFileSync as run, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";

const { chromium } = createRequire(new URL("../e2e/package.json", import.meta.url))(
  "@playwright/test",
);
const SHOP = "http://demo-sk.localhost:8080";
// Unique per run: never touch another run's container; Docker picks a free loopback port.
const CONTAINER = `autofill-vaultwarden-${process.pid}-${crypto.randomBytes(3).toString("hex")}`;
let VW = "";
const EMAIL = "jan@example.com";
const PASSWORD = "Test-Password-123!";
const ID = {
  firstName: "Ján",
  lastName: "Novák",
  address1: "Hlavná 12",
  city: "Bratislava",
  postalCode: "81101",
  country: "SK",
  email: "jan.novak@example.com",
  phone: "+421900123456",
};
const WANT = {
  email: ID.email,
  tel: ID.phone,
  name: `${ID.firstName} ${ID.lastName}`,
  organization: "",
  "street-address": ID.address1,
  "postal-code": ID.postalCode,
  city: ID.city,
  country: ID.country,
};
const ship = Boolean(process.env.SHIP);
const dir = mkdtempSync(join(tmpdir(), "autofill-"));
const sh = (cmd, args) => run(cmd, args, { cwd: dir, stdio: ["ignore", "pipe", "pipe"] });
process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0"; // self-signed throwaway server on loopback

// Bitwarden client-side crypto, just enough to register and create one item.
const hkdf = (prk, info) =>
  crypto
    .createHmac("sha256", prk)
    .update(Buffer.from(`${info}\x01`))
    .digest();
const enc = (data, key) => {
  const iv = crypto.randomBytes(16);
  const c = crypto.createCipheriv("aes-256-cbc", key.subarray(0, 32), iv);
  const ct = Buffer.concat([c.update(data), c.final()]);
  const mac = crypto
    .createHmac("sha256", key.subarray(32))
    .update(Buffer.concat([iv, ct]))
    .digest();
  return `2.${iv.toString("base64")}|${ct.toString("base64")}|${mac.toString("base64")}`;
};
const api = async (path, init) => {
  const r = await fetch(VW + path, init);
  if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
  return r.json();
};

async function vault() {
  sh("openssl", [
    "req",
    "-x509",
    "-newkey",
    "rsa:2048",
    "-nodes",
    "-keyout",
    "key.pem",
    "-out",
    "cert.pem",
    "-days",
    "1",
    "-subj",
    "/CN=localhost",
    "-addext",
    "subjectAltName=DNS:localhost",
  ]);
  run("chmod", ["644", join(dir, "key.pem")]);
  sh("docker", [
    "run",
    "-d",
    "--name",
    CONTAINER,
    "-p",
    "127.0.0.1::80",
    "-v",
    `${dir}:/ssl:ro`,
    "-e",
    "SIGNUPS_ALLOWED=true",
    "-e",
    "I_REALLY_WANT_VOLATILE_STORAGE=true",
    "-e",
    'ROCKET_TLS={certs="/ssl/cert.pem",key="/ssl/key.pem"}',
    "vaultwarden/server",
  ]);
  created = true;
  const port = spawnSync("docker", ["port", CONTAINER, "80/tcp"], { encoding: "utf8" })
    .stdout.trim()
    .split(":")
    .pop();
  VW = `https://localhost:${port}`;
  for (const deadline = Date.now() + 60_000; ; ) {
    try {
      if ((await fetch(`${VW}/alive`, { signal: AbortSignal.timeout(2000) })).ok) break;
    } catch {}
    if (Date.now() > deadline) throw new Error("vaultwarden did not start within 60 s");
    await new Promise((r) => setTimeout(r, 500));
  }
  const master = crypto.pbkdf2Sync(PASSWORD, EMAIL, 600000, 32, "sha256");
  const hash = crypto.pbkdf2Sync(master, PASSWORD, 1, 32, "sha256").toString("base64");
  const userKey = crypto.randomBytes(64);
  const rsa = crypto.generateKeyPairSync("rsa", {
    modulusLength: 2048,
    publicKeyEncoding: { type: "spki", format: "der" },
    privateKeyEncoding: { type: "pkcs8", format: "der" },
  });
  const json = { "content-type": "application/json" };
  await api("/identity/accounts/register", {
    method: "POST",
    headers: json,
    body: JSON.stringify({
      email: EMAIL,
      masterPasswordHash: hash,
      key: enc(userKey, Buffer.concat([hkdf(master, "enc"), hkdf(master, "mac")])),
      kdf: 0,
      kdfIterations: 600000,
      keys: {
        publicKey: rsa.publicKey.toString("base64"),
        encryptedPrivateKey: enc(rsa.privateKey, userKey),
      },
    }),
  });
  const { access_token } = await api("/identity/connect/token", {
    method: "POST",
    body: new URLSearchParams({
      grant_type: "password",
      username: EMAIL,
      password: hash,
      scope: "api offline_access",
      client_id: "web",
      deviceType: "9",
      deviceIdentifier: crypto.randomUUID(),
      deviceName: "autofill-check",
    }),
  });
  const s = (v) => enc(Buffer.from(v), userKey);
  await api("/api/ciphers", {
    method: "POST",
    headers: { ...json, authorization: `Bearer ${access_token}` },
    body: JSON.stringify({
      type: 4,
      name: s("Jan Novak SK"),
      identity: Object.fromEntries(Object.entries(ID).map(([k, v]) => [k, s(v)])),
    }),
  });
}

async function toCheckout(page) {
  const model = await (
    await page.request.get(`${SHOP}/_p/public/pages/product/ruksak-roll-top`)
  ).json();
  for (const v of model.product.variants) {
    const r = await page.request.post(`${SHOP}/_p/cart/lines`, {
      headers: { origin: SHOP },
      data: { variant_id: v.id, quantity: 1 },
    });
    if (r.status() !== 409) break;
  }
  await page.goto(`${SHOP}/`);
  await Promise.all([
    page.waitForURL(/checkout\./),
    page.evaluate(() => {
      const f = document.createElement("form");
      f.method = "post";
      f.action = "/_p/checkout/start";
      document.body.append(f);
      f.submit();
    }),
  ]);
  await page.getByRole("heading", { level: 1, name: "Pokladňa" }).waitFor();
  await page.waitForTimeout(1500); // hydration
  if (ship) await page.getByText("Doručiť na inú adresu").click();
  // Only SK ships from this shop: add a probe option so a country fill is observable.
  await page.evaluate(() =>
    document.querySelectorAll("select[name=country]").forEach((s) => {
      s.add(new Option("Probe", "XX"));
      s.value = "XX";
    }),
  );
}

let failed = false;
async function report(label, page) {
  const rows = await page.evaluate(() =>
    [...document.querySelectorAll("form input[name], form select[name]")]
      .filter((e) => !["radio", "checkbox", "hidden"].includes(e.type))
      .map((e) => ({ name: e.name, autocomplete: e.getAttribute("autocomplete"), value: e.value })),
  );
  for (const r of rows) r.ok = r.value === WANT[r.name] ? "ok" : "WRONG";
  console.log(label);
  console.table(rows);
  // Bitwarden skips fields outside the viewport, so the contact block may stay empty when the
  // delivery address is open; only address fields are asserted then.
  failed ||= rows.some((r) => r.ok !== "ok" && !(ship && ["email", "tel"].includes(r.name)));
}

async function bitwarden() {
  const [tag] = sh("gh", ["release", "list", "-R", "bitwarden/clients", "-L", "30"])
    .toString()
    .match(/browser-v[\d.]+/);
  sh("gh", ["release", "download", tag, "-R", "bitwarden/clients", "-p", "dist-chrome-*.zip"]);
  sh("sh", ["-c", "unzip -q dist-chrome-*.zip -d ext"]);
  const ext = join(dir, "ext");
  const ctx = await chromium.launchPersistentContext(join(dir, "profile"), {
    headless: false,
    args: [
      `--disable-extensions-except=${ext}`,
      `--load-extension=${ext}`,
      "--ignore-certificate-errors",
    ],
  });
  try {
    const sw = ctx.serviceWorkers()[0] ?? (await ctx.waitForEvent("serviceworker"));
    const p = await ctx.newPage();
    await p.goto(`chrome-extension://${new URL(sw.url()).host}/popup/index.html?uilocation=popout`);
    // First run: "default password manager" prompt, then the intro carousel.
    await p.getByRole("button", { name: "Skip" }).click({ timeout: 60000 });
    // The carousel sometimes ignores a click made while it is still settling: retry.
    const email = p.getByLabel(/Email address/);
    for (let i = 0; i < 5 && !(await email.isVisible()); i++) {
      await p
        .getByRole("button", { name: "Log in", exact: true })
        .click()
        .catch(() => {});
      await email.waitFor({ timeout: 3000 }).catch(() => {});
    }
    await p.getByRole("button", { name: /bitwarden\.com/ }).click();
    await p
      .getByText(/self-hosted/i)
      .last()
      .click();
    await p.getByLabel("Server URL").fill(VW);
    await p.getByRole("button", { name: "Save" }).click();
    await email.fill(EMAIL);
    await p.getByRole("button", { name: "Continue" }).click();
    await p
      .getByLabel(/Master password/)
      .first()
      .fill(PASSWORD);
    await p.keyboard.press("Enter");
    await p.getByText("Jan Novak SK").first().waitFor({ timeout: 30000 });
    console.log(`Bitwarden ${tag} logged in`);

    const page = await ctx.newPage();
    await toCheckout(page);
    await page
      .getByLabel(/Meno a priezvisko/)
      .first()
      .click();
    // The inline menu lives in a closed shadow root: click its first row by position.
    let list;
    for (let i = 0; i < 40 && !list; i++) {
      list = page.frames().find((f) => f.url().includes("menu-list.html"));
      await page.waitForTimeout(250);
    }
    await page.waitForTimeout(1000);
    const box = await (await list.frameElement()).boundingBox();
    await page.mouse.click(box.x + 80, box.y + 30);
    await page.waitForTimeout(1500);
    await report("Bitwarden inline menu, identity 'Jan Novak SK':", page);
  } finally {
    await ctx.close();
  }
}

async function native() {
  const browser = await chromium.launch({ headless: false });
  try {
    const page = await browser.newPage();
    await toCheckout(page);
    const cdp = await page.context().newCDPSession(page);
    const { root } = await cdp.send("DOM.getDocument");
    const { nodeId } = await cdp.send("DOM.querySelector", {
      nodeId: root.nodeId,
      selector: ship ? 'input[autocomplete="shipping name"]' : "input[name=name]",
    });
    const { node } = await cdp.send("DOM.describeNode", { nodeId });
    const snapshot = () =>
      page.evaluate(() =>
        [...document.querySelectorAll("form input[name], form select[name]")]
          .filter((e) => !["radio", "checkbox", "hidden"].includes(e.type))
          .map((e) => ({
            name: e.name,
            autocomplete: e.getAttribute("autocomplete"),
            value: e.value,
          })),
      );
    const before = ship ? await snapshot() : [];
    const fields = {
      NAME_FULL: WANT.name,
      ADDRESS_HOME_STREET_ADDRESS: ID.address1,
      ADDRESS_HOME_CITY: ID.city,
      ADDRESS_HOME_ZIP: ID.postalCode,
      ADDRESS_HOME_COUNTRY: ID.country,
      EMAIL_ADDRESS: ID.email,
      PHONE_HOME_WHOLE_NUMBER: ID.phone,
    };
    await cdp.send("Autofill.trigger", {
      fieldId: node.backendNodeId,
      address: { fields: Object.entries(fields).map(([name, value]) => ({ name, value })) },
    });
    await page.waitForTimeout(1500);
    // With a delivery address, Chromium fills only the focused section: billing stays empty.
    if (ship) {
      const rows = await snapshot();
      console.log("Chromium native autofill, delivery section:");
      console.table(rows);
      failed ||= rows.some((r, i) =>
        r.autocomplete?.startsWith("shipping ")
          ? r.value !== WANT[r.name]
          : r.value !== before[i]?.value,
      );
    } else await report("Chromium native address autofill:", page);
  } finally {
    await browser.close();
  }
}

let created = false;
let cleaned = false;
function cleanup() {
  if (cleaned) return;
  cleaned = true;
  if (created) {
    const rm = spawnSync("docker", ["rm", "-f", CONTAINER], { encoding: "utf8" });
    if (rm.status !== 0) console.error(`could not remove ${CONTAINER}: ${rm.stderr.trim()}`);
  }
  rmSync(dir, { recursive: true, force: true });
}
for (const sig of ["SIGINT", "SIGTERM"])
  process.on(sig, () => {
    cleanup();
    process.exit(130);
  });

try {
  await vault();
  await bitwarden();
  await native();
} finally {
  cleanup();
}
console.log(failed ? "FAIL" : "PASS");
process.exit(failed ? 1 : 0);
