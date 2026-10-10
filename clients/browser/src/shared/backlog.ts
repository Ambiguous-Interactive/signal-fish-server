/** Count UTF-8 bytes without allocating a second payload buffer. */
export function utf8Bytes(text: string): number {
  let bytes = 0;
  for (let index = 0; index < text.length; index += 1) {
    const code = text.charCodeAt(index);
    if (code < 0x80) bytes += 1;
    else if (code < 0x800) bytes += 2;
    else if (
      code >= 0xd800 &&
      code <= 0xdbff &&
      index + 1 < text.length &&
      text.charCodeAt(index + 1) >= 0xdc00 &&
      text.charCodeAt(index + 1) <= 0xdfff
    ) {
      bytes += 4;
      index += 1;
    } else bytes += 3;
  }
  return bytes;
}

/** Credits follow retained work until consumption or explicit discard. */
export class Backlog {
  private count = 0;
  private bytes = 0;

  constructor(
    readonly maxCount: number,
    readonly maxBytes: number,
  ) {}

  reserve(bytes: number): (() => void) | null {
    if (
      !Number.isSafeInteger(bytes) ||
      bytes < 0 ||
      this.count >= this.maxCount ||
      bytes > this.maxBytes - this.bytes
    )
      return null;
    this.count += 1;
    this.bytes += bytes;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.count -= 1;
      this.bytes -= bytes;
    };
  }
}
