# Onboard profiles: format 7, backup and save path

The read-only profile dump was followed by controlled write/restore experiments on user sectors
0x0005 and 0x0001; both were restored byte-exact. The report-rate byte is settled (0x8061 rate
index) and the user-facing `save-to-mouse` path (CLI + GUI) was implemented and verified on the
real mouse, then sector 0x0001 was restored byte-exact from the first backup.

Target: change only stage 0 (X/Y/LOD), the report-rate byte and the two sleep timeout words of
**one user profile (sector 0x0001)**. Never touch the profile directory, factory/ROM sectors, DPI
index bytes, buttons, lighting or the unknown tail.

## What was read-verified on the real PRO X 2 (format 7)

- Description: `11 01 0D 0A 01 07 01 05 01 05 10 00 FF 0A 04 00 ...`
  - memory model 1, profile format **0x07**, macro format 1, 5 profiles, 1 factory profile,
    5 buttons, 16 sectors, sector size 255, mechanical layout 0x0A, various info 0x04.
- User directory sector `0x0000`: CRC **MATCH** (`0x4037`). Entries:
  `0x0001 enabled`, `0x0002..0x0005 disabled`, reserved byte `0xFF`, end marker `0xFFFF`.
- User profile sectors `0x0001..0x0005`: CRC **MATCH** (`0x5A96` for the active profile,
  `0x84DB` for the four factory-like profiles).
- Active profile: sector `0x0001`; current profile index (fn 0x40) = 1; current DPI index
  (fn 0xB0) = 0; onboard mode = 1.
- Active profile decoded (format 7):
  - byte 0 report rate raw `0x03` (at 1000 Hz live)
  - byte 1 default DPI index 3 (points at an empty stage)
  - byte 2 shifted DPI index 0
  - byte 3 active DPI index 0
  - stage 0 = X 1600 / Y 1600 / LOD 1; stages 1..4 = 0 / 0 / LOD 0
  - LOD comes from the 5th byte of each 5-byte stage.
- The live onboard DPI 1600/LOD 1 comes from **active index 0**, not from default index 3.
  Changing byte 1 alone therefore may not change the live DPI until the active index changes.

## Format 7 profile sector layout (255 bytes, validated against Solaar PR #3345 + dumps)

| Offset | Size | Meaning |
|---|---|---|
| 0x00 | 1 | report rate index (0x8061) |
| 0x01 | 1 | default DPI index (0..4) |
| 0x02 | 1 | shifted DPI index (0..4) |
| 0x03 | 1 | active DPI index (0..4) |
| 0x04 | 25 | 5 stages, each `X u16 LE`, `Y u16 LE`, `LOD u8` |
| 0x1D | 15 | reserved |
| 0x2C | 2 | powersave timeout, u16 LE (0x003C = 60) |
| 0x2E | 2 | poweroff timeout, u16 LE (0x012C = 300) |
| 0x30 | 64 | 16 button bindings, 4 bytes each |
| 0x70 | 141 | unknown tail (keep byte-for-byte) |
| 0xFD | 2 | CRC-16-CCITT, big-endian |

- CRC: **CRC-16-CCITT-FALSE**, init `0xFFFF`, polynomial `0x1021`, no reflection, xorout 0,
  over bytes `0x00..0xFC`; stored big-endian at `0xFD..0xFE`.
- `0x07` is **not** supported by the base libratbag/Solaar master formats (1..5). The layout
  above comes from the open Solaar PR #3345, cross-checked against the dumps.
- Report-rate encoding is **settled: byte 0 is the 0x8061 rate index**
  (`0..6 = 125..8000 Hz`). Writing `0x04` turned the live onboard rate into 2000 Hz; restoring
  `0x03` returned it to 1000 Hz. An interval-ms reading is refuted.

## Allowed functions (source name / wire function nibble)

Wire byte 3 = `(function << 4) | SWID` (`SWID = 0x0A`).

| Source name | Wire fn | Params | Use |
|---|---|---|---|
| 0x00 get description | 0 | none | read description |
| 0x20 get mode | 2 | none | read mode (`1` onboard, `2` host) |
| 0x30 set current profile | 3 | `[0, profile]` (libratbag sends `parameters[1] = index + 1`) | select profile; **not exercised yet** |
| 0x40 get current profile | 4 | none | `data[5]` = active profile index |
| 0x50 memory read | 5 | `[sector hi, lo, offset hi, lo]` | read 16 bytes |
| 0x60 memory address write | 6 | `[sector hi, lo, sub hi, lo, count hi, lo]` | begin write |
| 0x70 memory write | 7 | 16 data bytes | write one chunk |
| 0x80 write end | 8 | none | finish write |
| 0xB0 get current DPI index | 11 | none | `data[4]` |

`0x50`/`0x60`/`0x70`/`0x80`/`0xB0` are the address numbers used by libratbag; the wire
function is the high nibble (`5`, `6`, `7`, `8`, `11`).

## Exact write sequence for one user profile

Preconditions: mouse awake, G HUB closed, mouse in onboard mode (`fn 2 = 1`), target sector is
one of `0x0001..0x0005`, and a fresh backup exists.

1. Read description (`fn 0`) and verify profile format 7 and sector size 255.
2. Read mode (`fn 2`). If it is not 1, stop and report; **do not** change mode silently.
3. Read the user directory (`sector 0x0000`, 16 reads) and verify CRC. Confirm the target
   sector is listed and note its enabled flag. Do not modify the directory.
4. Read the target sector (16 reads of 16 bytes; the last read starts at offset 239 and only
   bytes 239..254 are new). Verify its CRC. Save a new backup before changing anything.
5. Copy the sector and modify only:
   - stage X/Y words at offsets `0x04 + 5*i` and `0x06 + 5*i`, and the LOD byte,
   - byte 0 (report rate) = 0x8061 rate index,
   - the powersave/poweroff words at 0x2C/0x2E (sleep/deep-sleep timers).
   Preserve every other byte, including the DPI index bytes, buttons, reserved bytes and the
   unknown tail.
6. Recompute CRC-16-CCITT over `0x00..0xFC` and store it big-endian at `0xFD..0xFE`.
7. Begin write (`fn 6`): `[sector_hi, sector_lo, 0, 0, 0, 255]`.
8. Write 16 chunks (`fn 7`):
   - chunks 0..14: bytes `0..239`, 16 bytes each;
   - chunk 15: bytes `240..254` (15 bytes) plus one `0x00` padding byte.
   Solaar writes exactly this way; libratbag's loop reads one byte past its 255-byte buffer,
   which must not be copied.
9. Write end (`fn 8`, no params).
10. Read the sector back (`fn 5`, 16 reads). Compare all 255 bytes with the intended image and
    verify the CRC. Do not accept a partial match.
11. If the read-back fails, retry the read up to 3 times. If it still fails or mismatches, run
    the restore sequence below instead of writing again blindly.
12. Record the result. A read-back match proves the stored sector, not that the firmware has
    switched the live DPI/rate. Check `fn 2`, `fn 0x40`, `fn 0xB0`, `status`/`0x2202`/`0x8061`
    as read-only confirmation.

## Restore from the backup

Backup container `backups/profiles-<stamp>.bin` (little-endian fields):

- `0..7` magic `GPXPROF1`
- `8` version `1`; `9` reserved
- `10..11` sector size; `12` mode; `13` current profile; `14` current DPI index; `15` reserved
- `16..35` description reply; `36..55` mode reply; `56..75` current-profile reply;
  `76..95` current-DPI reply
- `96..97` entry count
- then repeated: `sector u16 LE`, `length u16 LE`, raw sector bytes.

`gpx-tool profile-dump` writes these containers plus a human-readable `.txt` with the decoded
fields and a hex dump. `gpx-tool restore-sector <file> --yes` restores sector 0x0001 byte-for-byte
from a raw 255-byte sector backup (its CRC is already inside those bytes). It uses the same
`fn 6` / `fn 7` / `fn 8` sequence, then reads back and compares all 255 bytes. The GUI makes a
timestamped raw sector backup in `backups/` before every **Save to mouse**.

Never restore the directory or ROM sectors unless a later, separately approved task says so.
Keep the `.txt` file next to the `.bin`; it contains the decoded fields and the hex dump for
human inspection.

## Risks and unverified steps

- **The write steps were verified** for user sectors 0x0005 and 0x0001 in onboard mode. Directory
  and ROM writes remain forbidden and untested; host-mode writes are untested.
- Format 7 is from an open, unmerged Solaar PR; it was hardware-tested on a G304 X, not this
  Superlight 2. The offsets match the dumps and the user-sector CRCs match.
- Report-rate byte encoding is resolved (0x8061 rate index); it was changed successfully and
  restored. Only `save_to_mouse` (explicit user action) may write it, and only in sector 0x0001.
- The last 16-byte chunk is 15 data bytes plus a `0x00` pad; firmware may reject or ignore the
  pad. This is a likely failure point.
- Whether the write requires onboard mode, host mode, or either is unknown. The implementation
  switches to onboard mode because the target is onboard memory.
- Changing the default index does not necessarily change the active index. A separate
  set-current-DPI-index (`0xC0` / wire fn 12) is a different write and is **not** part of this
  path; without it, the live DPI may stay on the active index until a profile switch, power
  cycle or DPI-button cycle.
- A power loss, unplug or crash between `fn 6` and `fn 8` can leave the sector partially
  written. There is no journaling. The only rollback is the backup file.
- The write may wear flash; retry loops increase that risk.
- The ROM sectors (`0x0100`, `0x0101`) had checksum **MISMATCH** (`0xA80E`, `0xFFFF`) and their
  format 7 layout is not documented. Never write them. Factory sectors may store `0000`/`FFFF`
  instead of a CRC, so this is not automatically a read error.
- G HUB or another Logitech service can swallow replies; close it first.

## Verification results

- The write sequence was exercised in **onboard mode** on two user sectors. Every chunk (15 x 16
  bytes plus 240..254 + `0x00` pad) was accepted; the `0x00` pad did not cause an error.
- Sector `0x0005` (disabled profile 5): stage 0 changed from 800/800 LOD 2 to 900/900 LOD 2.
  Intended and read-back CRC **0x9A2B**; read-back byte-exact after one write. Restored to the
  backup CRC **0x84DB**, read-back byte-exact.
- Sector `0x0001` (active profile 1): stage 0 changed from 1600/1600 LOD 1 to 800/800 LOD 1.
  Intended and read-back CRC **0xC4B9**; read-back byte-exact after one write. Restored to the
  backup CRC **0x5A96**, read-back byte-exact.
- **The written active profile applied immediately:** the live 0x2202 fn 5 read was 800/800 LOD 1
  right after the sector 0x0001 write, and 1600/1600 LOD 1 right after the restore. No
  host->onboard cycle and no set-current-profile (0x30 / wire fn 3) call were needed. The
  set-current-profile path therefore remains untested.
- A fresh read-only dump after the test showed sectors 0x0001 and 0x0005 byte-identical to the
  earlier backup; the user directory CRC still matched; mode onboard, live DPI 1600/1600 LOD 1,
  rate 1000 Hz.
- **The report-rate byte (0x00) is the 0x8061 rate index.** A fresh-read sector 0x0001
  (CRC 0x5A96, byte 0x03) was backed up, byte 0x00 was changed to 0x04 (CRC 0x3863), written,
  and read back byte-exact. In onboard mode the live rate (fn 2, connection 1) became **2000 Hz**
  (`11 01 0C 2A 04 ...`). The sector was restored byte-exact (CRC 0x5A96) and the live rate
  returned to **1000 Hz**. The interval-ms reading is refuted.
- **Save to mouse** fresh-reads sector 0x0001, switches to onboard mode first (important: in host
  mode fn 4 reports current profile 0), requires profile 1, validates DPI/LOD/rate against the
  live getters, writes a timestamped raw backup, changes only the requested fields, recomputes
  the CRC, writes, verifies byte-exact read-back and live DPI/rate, and restores the backup
  automatically on any problem. At most the 20 newest `profile-*-sector0001.bin` files are kept.
- The GUI path was verified with a debug build: the footer's **Save to mouse** button opened the
  inline confirmation `Write 800 DPI · Low · 2000 Hz to the mouse's memory?`; **Save** sent
  `Command::SaveToMouse` on the worker thread. Result: mode onboard, live 800/800 LOD 1 and
  2000 Hz, sector CRC **0xA64C**, read-back byte-exact. The button is disabled while asleep or
  not connected, and no write happens without the explicit Save click.
- **Restore:** sector 0x0001 was restored byte-exact from the first backup (CRC 0x5A96); final
  state onboard, 1600/1600, LOD 1, 1000 Hz. The test settings and Run value were restored
  afterwards.
