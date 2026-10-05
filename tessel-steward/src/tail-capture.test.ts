import { describe, expect, it } from "vitest";

import { captureTail, TailBuffer } from "./tail-capture";

const encode = (text: string): Uint8Array => new TextEncoder().encode(text);

function streamOf(chunks: Uint8Array[]): ReadableStream {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  });
}

describe("TailBuffer", () => {
  it("keeps everything under the limit", () => {
    const buffer = new TailBuffer(10);
    buffer.push(encode("abc"));
    expect(buffer.result()).toEqual({ text: "abc", truncated: false });
  });

  it("keeps everything exactly at the limit and does not flag it", () => {
    const buffer = new TailBuffer(5);
    buffer.push(encode("abcde"));
    expect(buffer.result()).toEqual({ text: "abcde", truncated: false });
  });

  it("keeps the end, not the start, one byte over the limit", () => {
    const buffer = new TailBuffer(5);
    buffer.push(encode("abcdef"));
    expect(buffer.result()).toEqual({ text: "bcdef", truncated: true });
  });

  it("keeps the end across many chunks", () => {
    const buffer = new TailBuffer(4);
    for (const piece of ["ab", "cd", "e", "fgh", "i"]) {
      buffer.push(encode(piece));
    }
    expect(buffer.result()).toEqual({ text: "fghi", truncated: true });
  });

  it("handles one chunk larger than the limit after smaller ones", () => {
    const buffer = new TailBuffer(3);
    buffer.push(encode("xy"));
    buffer.push(encode("0123456789"));
    expect(buffer.result()).toEqual({ text: "789", truncated: true });
  });

  it("returns empty text for no input", () => {
    expect(new TailBuffer(3).result()).toEqual({ text: "", truncated: false });
  });

  it("drops a multi-byte character split by the cut instead of emitting garbage", () => {
    const buffer = new TailBuffer(4);
    buffer.push(encode("a€bc"));
    const { text, truncated } = buffer.result();
    expect(truncated).toBe(true);
    expect(text).toBe("bc");
    expect(text).not.toContain("�");
  });

  it("keeps a whole multi-byte character that fits exactly", () => {
    const buffer = new TailBuffer(5);
    buffer.push(encode("a€bc"));
    expect(buffer.result().text).toBe("€bc");
  });

  it("decodes a cut inside a four-byte character without throwing", () => {
    const buffer = new TailBuffer(3);
    buffer.push(encode("😀z"));
    expect(buffer.result().text).not.toContain("�");
  });

  it("rejects a limit that is not a positive integer", () => {
    expect(() => new TailBuffer(0)).toThrow("positive integer");
    expect(() => new TailBuffer(1.5)).toThrow("positive integer");
  });
});

describe("captureTail", () => {
  it("reads a multi-chunk stream to the end and keeps the tail", async () => {
    const stream = streamOf([encode("hello "), encode("wor"), encode("ld")]);
    expect(await captureTail(stream, 7)).toEqual({ text: "o world", truncated: true });
  });

  it("returns empty text for an empty stream", async () => {
    expect(await captureTail(streamOf([]), 7)).toEqual({ text: "", truncated: false });
  });
});
