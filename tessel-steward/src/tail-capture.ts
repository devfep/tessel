const UTF8_CONTINUATION_MASK = 0b1100_0000;
const UTF8_CONTINUATION_BITS = 0b1000_0000;
const UTF8_MAX_SEQUENCE_LENGTH = 4;

/** Text kept from a byte stream, and whether earlier bytes were dropped. */
export interface TailCapture {
  text: string;
  truncated: boolean;
}

/**
 * Keeps the last `limit` bytes of a stream of chunks without holding more than `limit` bytes
 * plus one chunk.
 *
 * When bytes were dropped, the kept tail starts at the first complete UTF-8 character, so the
 * text never begins with a partial character.
 */
export class TailBuffer {
  private tail = new Uint8Array(0);
  private dropped = false;

  constructor(private readonly limit: number) {
    if (!Number.isInteger(limit) || limit < 1) {
      throw new Error(`TailBuffer limit must be a positive integer, got ${limit}`);
    }
  }

  push(chunk: Uint8Array): void {
    const joined = new Uint8Array(this.tail.length + chunk.length);
    joined.set(this.tail);
    joined.set(chunk, this.tail.length);
    if (joined.length > this.limit) {
      this.dropped = true;
      this.tail = joined.slice(joined.length - this.limit);
    } else {
      this.tail = joined;
    }
  }

  result(): TailCapture {
    let start = 0;
    if (this.dropped) {
      const scanEnd = Math.min(this.tail.length, UTF8_MAX_SEQUENCE_LENGTH - 1);
      while (start < scanEnd && isContinuationByte(this.tail[start])) {
        start += 1;
      }
    }
    const text = new TextDecoder().decode(this.tail.subarray(start));
    return { text, truncated: this.dropped };
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
export async function captureTail(stream: ReadableStream, limit: number): Promise<TailCapture> {
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
