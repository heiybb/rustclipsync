export class SyncRoom implements DurableObject {
  constructor(
    private state: DurableObjectState,
    private env: unknown,
  ) {}

  async fetch(): Promise<Response> {
    return new Response("not implemented", { status: 501 });
  }
}
