import { SyncRoom } from "./room";

export interface Env {
  ROOM: DurableObjectNamespace;
  OBJECTS: R2Bucket;
  AUTH_TOKEN: string;
}

export { SyncRoom };

export default {
  async fetch(request: Request, _env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok" });
    }

    return new Response("not found", { status: 404 });
  },
};
