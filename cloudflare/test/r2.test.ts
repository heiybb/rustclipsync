import { describe, expect, it } from "vitest";
import { isAuthorized, parseObjectPath } from "../src/r2";

describe("r2 endpoints", () => {
  it("accepts matching bearer token", () => {
    const request = new Request("https://example.com/objects/m1", {
      headers: { authorization: "Bearer secret" },
    });

    expect(isAuthorized(request, "secret")).toBe(true);
  });

  it("rejects missing bearer token", () => {
    const request = new Request("https://example.com/objects/m1");

    expect(isAuthorized(request, "secret")).toBe(false);
  });

  it("parses object message ids", () => {
    expect(parseObjectPath("/objects/message-1")).toEqual({
      messageId: "message-1",
    });
  });

  it("rejects non-object paths", () => {
    expect(parseObjectPath("/other/message-1")).toBeNull();
  });
});
