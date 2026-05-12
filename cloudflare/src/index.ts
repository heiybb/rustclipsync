import { SyncRoom } from "./room";
import { DEFAULT_ROOM } from "./protocol";
import {
  getObject,
  isAuthorized,
  objectKeyForUpload,
  parseObjectPath,
  putObject,
} from "./r2";

export interface Env {
  ROOM: DurableObjectNamespace;
  OBJECTS: R2Bucket;
  AUTH_TOKEN: string;
}

export { SyncRoom };

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok" });
    }

    if (url.pathname === "/ws") {
      if (!isAuthorized(request, env.AUTH_TOKEN)) {
        return new Response("unauthorized", { status: 401 });
      }
      const id = env.ROOM.idFromName(DEFAULT_ROOM);
      return env.ROOM.get(id).fetch(request);
    }

    const objectPath = parseObjectPath(url.pathname);
    if (objectPath) {
      if (!isAuthorized(request, env.AUTH_TOKEN)) {
        return new Response("unauthorized", { status: 401 });
      }

      const filename = url.searchParams.get("filename");
      const key = objectKeyForUpload(objectPath.messageId, filename);
      if (request.method === "PUT") {
        return putObject(env.OBJECTS, key, request);
      }
      if (request.method === "GET") {
        return getObject(env.OBJECTS, key);
      }
      return new Response("method not allowed", { status: 405 });
    }

    return new Response("not found", { status: 404 });
  },
};
