import { describe, expect, it } from "vitest";

import { captureTail, TailBuffer } from "./tail-capture";

const encode = (text: string): Uint8Array => new TextEncoder().encode(text);

function streamOf(chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  });
}

function tailOf(limit: number, ...chunks: Uint8Array[]) {
  const buffer = new TailBuffer(limit);
  for (const chunk of chunks) {
    buffer.push(chunk);
  }
  return buffer.result();
}

describe("TailBuffer", () => {
  it("keeps everything under the limit", () => {
    expect(tailOf(10, encode("abc"))).toEqual({ text: "abc", truncated: false });
  });

  it("keeps everything exactly at the limit and does not flag it", () => {
    expect(tailOf(5, encode("abcde"))).toEqual({ text: "abcde", truncated: false });
  });

  it("keeps the end, not the start, one byte over the limit", () => {
    expect(tailOf(5, encode("abcdef"))).toEqual({ text: "bcdef", truncated: true });
  });

  it("keeps the end across many chunks", () => {
    const chunks = ["ab", "cd", "e", "fgh", "i"].map(encode);
    expect(tailOf(4, ...chunks)).toEqual({ text: "fghi", truncated: true });
  });

  it("handles one chunk larger than the limit after smaller ones", () => {
    expect(tailOf(3, encode("xy"), encode("0123456789"))).toEqual({
      text: "789",
      truncated: true,
    });
  });

  it("returns empty text for no input", () => {
    expect(tailOf(3)).toEqual({ text: "", truncated: false });
  });

  it("keeps the exact end after many small chunks far beyond the limit", () => {
    const limit = 1000;
    const chunks: Uint8Array[] = [];
    let all = "";
    for (let index = 0; index < 5000; index += 1) {
      const piece = `${index % 10}`.repeat((index % 7) + 1);
      all += piece;
      chunks.push(encode(piece));
    }
    expect(tailOf(limit, ...chunks)).toEqual({ text: all.slice(-limit), truncated: true });
  });

  it("is correct when a push lands exactly on the compaction boundary", () => {
    const text = tailOf(4, encode("abcd"), encode("efgh"), encode("i"), encode("jkl"));
    expect(text).toEqual({ text: "ijkl", truncated: true });
  });

  it("drops the rest of a 4-byte character cut after its lead byte", () => {
    expect(tailOf(4, encode("😀z"))).toEqual({ text: "z", truncated: true });
  });

  it("drops the rest of a 3-byte character cut after its lead byte", () => {
    expect(tailOf(3, encode("a€b"))).toEqual({ text: "b", truncated: true });
  });

  it("drops the last byte of a 2-byte character cut after its lead byte", () => {
    expect(tailOf(2, encode("éz"))).toEqual({ text: "z", truncated: true });
  });

  it("drops two continuation bytes when the cut leaves a 4-byte character's last two", () => {
    expect(tailOf(4, encode("😀"), encode("zz"))).toEqual({ text: "zz", truncated: true });
  });

  it("drops no character when the cut falls on a character boundary", () => {
    expect(tailOf(4, encode("a€b"))).toEqual({ text: "€b", truncated: true });
  });

  it("keeps a multi-byte character split across chunks that fits whole", () => {
    const bytes = encode("a€bc");
    expect(tailOf(6, bytes.subarray(0, 2), bytes.subarray(2))).toEqual({
      text: "a€bc",
      truncated: false,
    });
  });

  it("passes untruncated input that begins with continuation bytes through unchanged", () => {
    expect(tailOf(10, Uint8Array.of(0x80, 0x80, 0x41))).toEqual({
      text: "��A",
      truncated: false,
    });
  });

  it("skips at most three continuation bytes, as no UTF-8 sequence has more", () => {
    const bytes = Uint8Array.of(0x41, 0x80, 0x80, 0x80, 0x80, 0x80);
    expect(tailOf(5, bytes)).toEqual({ text: "\uFFFD\uFFFD", truncated: true });
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
