import { DEFAULT_ROOM, R2_LIMIT_BYTES, objectKeyFor } from "./protocol";

export type EnvWithObjects = {
  OBJECTS: R2Bucket;
};

export type ParsedObjectPath = {
  messageId: string;
};

export function isAuthorized(request: Request, token: string): boolean {
  return request.headers.get("authorization") === `Bearer ${token}`;
}

export function parseObjectPath(pathname: string): ParsedObjectPath | null {
  const match = pathname.match(/^\/objects\/([^/]+)$/);
  if (!match) {
    return null;
  }
  return { messageId: decodeURIComponent(match[1]) };
}

export function objectKeyForUpload(
  messageId: string,
  filename: string | null,
): string {
  return objectKeyFor(DEFAULT_ROOM, messageId, filename || "payload.bin");
}

export async function putObject(
  bucket: R2Bucket,
  key: string,
  request: Request,
): Promise<Response> {
  const length = Number(request.headers.get("content-length") || "0");
  if (!Number.isFinite(length) || length <= 0) {
    return new Response("missing content-length", { status: 411 });
  }
  if (length > R2_LIMIT_BYTES) {
    return new Response("payload too large", { status: 413 });
  }

  await bucket.put(key, request.body, {
    httpMetadata: {
      contentType:
        request.headers.get("content-type") || "application/octet-stream",
    },
  });

  return Response.json({ object_key: key, size: length });
}

export async function getObject(
  bucket: R2Bucket,
  key: string,
): Promise<Response> {
  const object = await bucket.get(key);
  if (!object) {
    return new Response("not found", { status: 404 });
  }

  return new Response(object.body, {
    headers: {
      "content-type":
        object.httpMetadata?.contentType || "application/octet-stream",
      "content-length": String(object.size),
    },
  });
}
