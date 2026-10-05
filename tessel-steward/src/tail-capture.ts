const UTF8_CONTINUATION_MASK = 0b1100_0000;
const UTF8_CONTINUATION_BITS = 0b1000_0000;
const UTF8_MAX_SEQUENCE_LENGTH = 4;

/** Text kept from a byte stream, and whether earlier bytes were dropped. */
export interface TailCapture {
  text: string;
  truncated: boolean;
}

/**
 * Keeps the last `limit` bytes of a stream of chunks.
 *
 * Bytes accumulate in a buffer of twice the limit and are compacted only when it fills, so each
 * byte is copied at most about twice and memory stays at 2 x limit plus one chunk. When bytes
 * were dropped, the kept tail starts at the first complete UTF-8 character, so the text never
 * begins with a partial character. Input that was not truncated is decoded as is.
 */
export class TailBuffer {
  private readonly buffer: Uint8Array;
  private length = 0;
  private total = 0;

  constructor(private readonly limit: number) {
    if (!Number.isInteger(limit) || limit < 1) {
      throw new Error(`TailBuffer limit must be a positive integer, got ${limit}`);
    }
    this.buffer = new Uint8Array(limit * 2);
  }

  push(chunk: Uint8Array): void {
    this.total += chunk.length;
    if (chunk.length >= this.limit) {
      this.buffer.set(chunk.subarray(chunk.length - this.limit));
      this.length = this.limit;
      return;
    }
    if (this.length + chunk.length > this.buffer.length) {
      this.buffer.copyWithin(0, this.length - this.limit, this.length);
      this.length = this.limit;
    }
    this.buffer.set(chunk, this.length);
    this.length += chunk.length;
  }

  result(): TailCapture {
    const truncated = this.total > this.limit;
    const end = this.length;
    let start = Math.max(0, end - this.limit);
    if (truncated) {
      const scanEnd = Math.min(end, start + UTF8_MAX_SEQUENCE_LENGTH - 1);
      while (start < scanEnd && isContinuationByte(this.buffer[start])) {
        start += 1;
      }
    }
    const text = new TextDecoder().decode(this.buffer.subarray(start, end));
    return { text, truncated };
  }
}

function isContinuationByte(byte: number | undefined): boolean {
  return byte !== undefined && (byte & UTF8_CONTINUATION_MASK) === UTF8_CONTINUATION_BITS;
}

/**
 * Reads a stream to its end and returns the last `limit` bytes as text.
 *
 * @param stream Process output stream.
 * @param limit Maximum number of bytes kept.
 */
export async function captureTail(
  stream: ReadableStream<Uint8Array>,
  limit: number,
): Promise<TailCapture> {
  const buffer = new TailBuffer(limit);
  const reader = stream.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  return buffer.result();
}
