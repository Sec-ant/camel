// Natural file-name order, byte-for-byte the same as the CLI (camel::comic::natural_cmp):
// digit runs compare numerically (leading zeros ignored), other runs compare as UTF-8 bytes;
// ties fall back to plain byte order.

const encoder = new TextEncoder();

function isDigit(byte: number): boolean {
  return byte >= 0x30 && byte <= 0x39;
}

function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const length = Math.min(a.length, b.length);
  for (let i = 0; i < length; i++) if (a[i] !== b[i]) return a[i]! - b[i]!;
  return a.length - b.length;
}

function naturalBytes(a: Uint8Array, b: Uint8Array): number {
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    const digitA = isDigit(a[i]!);
    const digitB = isDigit(b[j]!);
    let endA = i;
    while (endA < a.length && isDigit(a[endA]!) === digitA) endA++;
    let endB = j;
    while (endB < b.length && isDigit(b[endB]!) === digitB) endB++;
    let runA = a.subarray(i, endA);
    let runB = b.subarray(j, endB);
    let order: number;
    if (digitA && digitB) {
      let zerosA = 0;
      while (zerosA < runA.length && runA[zerosA] === 0x30) zerosA++;
      let zerosB = 0;
      while (zerosB < runB.length && runB[zerosB] === 0x30) zerosB++;
      runA = runA.subarray(zerosA);
      runB = runB.subarray(zerosB);
      order = runA.length !== runB.length ? runA.length - runB.length : compareBytes(runA, runB);
    } else {
      order = compareBytes(runA, runB);
    }
    if (order !== 0) return order;
    i = endA;
    j = endB;
  }
  return a.length - i - (b.length - j);
}

export function naturalCompare(a: string, b: string): number {
  const bytesA = encoder.encode(a);
  const bytesB = encoder.encode(b);
  return naturalBytes(bytesA, bytesB) || compareBytes(bytesA, bytesB);
}
