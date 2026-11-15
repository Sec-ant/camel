import assert from "node:assert/strict";
import { test } from "node:test";
import { crc32, zipBlob } from "../src/storage/zip.ts";

const encoder = new TextEncoder();

test("crc32 matches the IEEE check value and continues across chunks", () => {
  assert.equal(crc32(encoder.encode("123456789")), 0xcbf43926);
  assert.equal(crc32(encoder.encode("6789"), crc32(encoder.encode("12345"))), 0xcbf43926);
  assert.equal(crc32(new Uint8Array()), 0);
});

test("zipBlob writes a stored archive whose directory points at each entry", async () => {
  const entries = [
    { path: "00001.json", data: new Blob(["{\"a\":1}\n"]) },
    { path: "reports/音.json", data: new Blob([""]) },
  ];
  const zip = new Uint8Array(await (await zipBlob(entries, new Date(2026, 10, 15, 9, 30, 10))).arrayBuffer());
  const view = new DataView(zip.buffer);
  const end = zip.length - 22;
  assert.equal(view.getUint32(end, true), 0x06054b50);
  assert.equal(view.getUint16(end + 10, true), 2);
  let central = view.getUint32(end + 16, true);
  const decoder = new TextDecoder();
  for (const entry of entries) {
    assert.equal(view.getUint32(central, true), 0x02014b50);
    assert.equal(view.getUint16(central + 10, true), 0, "stored");
    const size = view.getUint32(central + 24, true);
    const nameLength = view.getUint16(central + 28, true);
    const local = view.getUint32(central + 42, true);
    assert.equal(decoder.decode(zip.subarray(central + 46, central + 46 + nameLength)), entry.path);
    assert.equal(view.getUint32(local, true), 0x04034b50);
    const start = local + 30 + view.getUint16(local + 26, true);
    const data = zip.subarray(start, start + size);
    assert.equal(decoder.decode(data), await entry.data.text());
    assert.equal(view.getUint32(central + 16, true), crc32(data));
    central += 46 + nameLength;
  }
});
