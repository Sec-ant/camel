// Stored (uncompressed) ZIP archives assembled as a Blob: entry data stays in the source
// Blobs (OPFS or IndexedDB-backed files are not copied into memory), only CRC-32 is computed
// by streaming each entry once. Used for "Download changes" and "Download project".

const TABLE = new Uint32Array(256).map((_, index) => {
  let value = index;
  for (let bit = 0; bit < 8; bit++) value = value & 1 ? 0xedb88320 ^ (value >>> 1) : value >>> 1;
  return value;
});

/** CRC-32 (IEEE) of `bytes`, continuing from a previous result `crc`. */
export function crc32(bytes: Uint8Array, crc = 0): number {
  let value = ~crc >>> 0;
  for (const byte of bytes) value = TABLE[(value ^ byte) & 0xff]! ^ (value >>> 8);
  return ~value >>> 0;
}

export interface ZipEntry {
  readonly path: string;
  readonly data: Blob;
}

const UTF8_NAMES = 0x0800;
const LIMIT = 0xffffffff;

function dosDateTime(date: Date): { time: number; date: number } {
  return {
    time: (date.getHours() << 11) | (date.getMinutes() << 5) | (date.getSeconds() >> 1),
    date: ((date.getFullYear() - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
  };
}

/** Builds a stored ZIP; throws for archives beyond the classic 4 GiB / 65535-entry format. */
export async function zipBlob(entries: readonly ZipEntry[], modified = new Date()): Promise<Blob> {
  if (entries.length > 0xffff) throw new Error("Too many files for a zip download");
  const stamp = dosDateTime(modified);
  const encoder = new TextEncoder();
  const parts: BlobPart[] = [];
  const central: Uint8Array<ArrayBuffer>[] = [];
  let offset = 0;
  for (const entry of entries) {
    let crc = 0;
    const reader = entry.data.stream().getReader();
    for (let chunk = await reader.read(); !chunk.done; chunk = await reader.read()) crc = crc32(chunk.value, crc);
    const name = encoder.encode(entry.path);
    const size = entry.data.size;
    if (offset + 30 + name.length + size > LIMIT) throw new Error("The files are too large for a zip download");

    const local = new Uint8Array(30 + name.length);
    const header = new DataView(local.buffer);
    header.setUint32(0, 0x04034b50, true);
    header.setUint16(4, 20, true);
    header.setUint16(6, UTF8_NAMES, true);
    header.setUint16(10, stamp.time, true);
    header.setUint16(12, stamp.date, true);
    header.setUint32(14, crc, true);
    header.setUint32(18, size, true);
    header.setUint32(22, size, true);
    header.setUint16(26, name.length, true);
    local.set(name, 30);

    const record = new Uint8Array(46 + name.length);
    const directory = new DataView(record.buffer);
    directory.setUint32(0, 0x02014b50, true);
    directory.setUint16(4, 20, true);
    directory.setUint16(6, 20, true);
    directory.setUint16(8, UTF8_NAMES, true);
    directory.setUint16(12, stamp.time, true);
    directory.setUint16(14, stamp.date, true);
    directory.setUint32(16, crc, true);
    directory.setUint32(20, size, true);
    directory.setUint32(24, size, true);
    directory.setUint16(28, name.length, true);
    directory.setUint32(42, offset, true);
    record.set(name, 46);

    parts.push(local, entry.data);
    central.push(record);
    offset += local.length + size;
  }
  const centralSize = central.reduce((sum, record) => sum + record.length, 0);
  const end = new Uint8Array(22);
  const trailer = new DataView(end.buffer);
  trailer.setUint32(0, 0x06054b50, true);
  trailer.setUint16(8, entries.length, true);
  trailer.setUint16(10, entries.length, true);
  trailer.setUint32(12, centralSize, true);
  trailer.setUint32(16, offset, true);
  return new Blob([...parts, ...central, end], { type: "application/zip" });
}
