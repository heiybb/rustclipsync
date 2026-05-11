import { describe, expect, it } from "vitest";
import {
  INLINE_LIMIT_BYTES,
  R2_LIMIT_BYTES,
  isValidClientMessage,
  objectKeyFor,
} from "../src/protocol";

describe("protocol limits", () => {
  it("uses the confirmed Cloudflare payload limits", () => {
    expect(INLINE_LIMIT_BYTES).toBe(10 * 1024 * 1024);
    expect(R2_LIMIT_BYTES).toBe(100 * 1024 * 1024);
  });
});

describe("client message validation", () => {
  it("accepts hello messages with client identity", () => {
    expect(
      isValidClientMessage({
        type: "hello",
        client_id: "RYZEN",
        client_name: "RYZEN",
        last_seen_sequence: 0,
      }),
    ).toBe(true);
  });
});

describe("R2 object keys", () => {
  it("builds deterministic R2 keys", () => {
    expect(objectKeyFor("default", "message-1", "sample.txt")).toBe(
      "rooms/default/messages/message-1/sample.txt",
    );
  });

  it("replaces dot-only path segments", () => {
    expect(objectKeyFor(".", "..", "...")).toBe(
      "rooms/unnamed/messages/unnamed/unnamed",
    );
  });
});
