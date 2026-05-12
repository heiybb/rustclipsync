import { describe, expect, it } from "vitest";
import { chooseCatchupMessages, messagesForClient } from "../src/room";
import type { RelayMessage } from "../src/protocol";

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

describe("room catchup", () => {
  it("returns messages after last seen when available", () => {
    expect(
      chooseCatchupMessages([msg(1), msg(2), msg(3)], 1).map(
        (message) => message.sequence,
      ),
    ).toEqual([2, 3]);
  });

  it("returns only latest state when client is too old", () => {
    expect(
      chooseCatchupMessages([msg(10), msg(11)], 1).map(
        (message) => message.sequence,
      ),
    ).toEqual([11]);
  });

  it("does not send messages back to their source", () => {
    expect(
      messagesForClient([msg(1, "a"), msg(2, "b")], "a").map(
        (message) => message.sequence,
      ),
    ).toEqual([2]);
  });
});
