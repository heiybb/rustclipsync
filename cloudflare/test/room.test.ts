import { describe, expect, it } from "vitest";
import { archivedMessage, catchupMessage, type StoredLatest } from "../src/room";
import type { RelayMessage } from "../src/protocol";

const NOW = 1_750_000_000_000;

function msg(sequence: number, source = "a"): RelayMessage {
  return {
    sequence,
    source,
    message_id: `m${sequence}`,
    kind: "text",
    payload_hash: `h${sequence}`,
    payload: { mode: "inline", bytes_base64: "aGVsbG8=" },
  };
}

function stored(sequence: number, source = "a"): StoredLatest {
  return { message: msg(sequence, source), expiresAt: NOW + 60_000 };
}

describe("room catchup", () => {
  it("returns the latest message for a client that is behind", () => {
    expect(catchupMessage(stored(5), 3, "b", NOW)?.sequence).toBe(5);
  });

  it("returns nothing when the store is empty", () => {
    expect(catchupMessage(undefined, 0, "b", NOW)).toBeNull();
  });

  it("returns nothing once the message has expired", () => {
    const latest = stored(5);
    expect(catchupMessage(latest, 0, "b", latest.expiresAt)).toBeNull();
  });

  it("returns nothing when the client has already seen it", () => {
    expect(catchupMessage(stored(5), 5, "b", NOW)).toBeNull();
  });

  it("does not send a message back to its source", () => {
    expect(catchupMessage(stored(5, "a"), 0, "a", NOW)).toBeNull();
  });
});

describe("inline payload archival", () => {
  it("rewrites an inline message as an r2 pointer", () => {
    const archived = archivedMessage(msg(5), "rooms/default/messages/m5/payload.bin", 5, 1_750_000_000);

    expect(archived.payload).toEqual({
      mode: "r2",
      object_key: "rooms/default/messages/m5/payload.bin",
      size: 5,
      expires_at: 1_750_000_000,
    });
    expect(archived.sequence).toBe(5);
    expect(archived.kind).toBe("text");
    expect(archived.payload_hash).toBe("h5");
  });

  it("leaves an r2 message untouched", () => {
    const original: RelayMessage = {
      ...msg(5),
      payload: { mode: "r2", object_key: "k", size: 9, expires_at: 1 },
    };

    expect(archivedMessage(original, "other", 5, 2)).toEqual(original);
  });
});
