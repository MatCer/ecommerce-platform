/**
 * Presigned image upload (spec §8.3, A21): `POST /assets/uploads` -> `PUT` the file to the
 * returned URL with exactly the returned headers -> `POST /assets/{id}/complete`. The worker
 * then renders the variants; callers poll the asset until `ready` or `failed`.
 */
import { api, idempotencyKey, type Schemas, tenantHeader, unwrap } from "./api.ts";

export type Asset = Schemas["Asset"];

export const ACCEPTED_TYPES = ["image/jpeg", "image/png", "image/webp", "image/gif"] as const;
export const MAX_BYTES = 20 * 1024 * 1024;

export type UploadCheck = "ok" | "unsupported_type" | "file_too_large";

export function checkFile(file: { type: string; size: number }): UploadCheck {
  if (!(ACCEPTED_TYPES as readonly string[]).includes(file.type)) return "unsupported_type";
  if (file.size > MAX_BYTES) return "file_too_large";
  return "ok";
}

/** PUT with progress events (fetch has no upload progress). */
function put(
  url: string,
  headers: Record<string, string>,
  file: File,
  onProgress: (fraction: number) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", url);
    for (const [k, v] of Object.entries(headers)) xhr.setRequestHeader(k, v);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress(e.loaded / e.total);
    };
    xhr.onload = () =>
      xhr.status >= 200 && xhr.status < 300
        ? resolve()
        : reject(new Error(`upload failed with ${xhr.status}`));
    xhr.onerror = () => reject(new Error("upload failed (network)"));
    xhr.send(file);
  });
}

export async function uploadImage(
  file: File,
  onProgress: (fraction: number) => void,
): Promise<Asset> {
  const { asset, upload } = await unwrap(
    api.POST("/admin/v1/assets/uploads", {
      params: { header: idempotencyKey() },
      body: { content_type: file.type, size: file.size, filename: file.name },
    }),
  );
  await put(upload.url, upload.headers, file, onProgress);
  onProgress(1);
  await unwrap(
    api.POST("/admin/v1/assets/{id}/complete", {
      params: { header: tenantHeader(), path: { id: asset.id } },
    }),
  );
  return asset;
}
