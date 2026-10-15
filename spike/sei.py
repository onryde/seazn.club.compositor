"""H.264 SEI user_data_unregistered helpers (C00 item 3, THROWAWAY)."""
import re

# 16-byte UUID with no zero bytes (so the payload needs no emulation prevention).
SEAZN_UUID = b"SEAZN-C00-SEI-v1"
assert len(SEAZN_UUID) == 16
# x264's own version SEI uses this UUID (dc45e9bd-e6d9-48b7-962c-d820d923eeef).
X264_UUID = bytes.fromhex("dc45e9bde6d948b7962cd820d923eeef")

START = re.compile(b"\x00\x00\x01")


def nal_units(data: bytes):
    """Split an Annex-B byte stream into NAL unit payloads (without start codes)."""
    idx = [m.start() for m in START.finditer(data)]
    out = []
    for n, i in enumerate(idx):
        s = i + 3
        e = idx[n + 1] if n + 1 < len(idx) else len(data)
        # a 4-byte start code leaves a trailing 0x00 on the previous NAL
        nal = data[s:e]
        if n + 1 < len(idx) and nal.endswith(b"\x00"):
            nal = nal[:-1]
        out.append((i, nal))
    return out


def ep_add(rbsp: bytes) -> bytes:
    out = bytearray()
    zeros = 0
    for b in rbsp:
        if zeros >= 2 and b <= 3:
            out.append(3)
            zeros = 0
        out.append(b)
        zeros = zeros + 1 if b == 0 else 0
    return bytes(out)


def ep_remove(ebsp: bytes) -> bytes:
    return re.sub(b"\x00\x00\x03", b"\x00\x00", ebsp)


def make_sei(text: str) -> bytes:
    payload = SEAZN_UUID + text.encode("ascii")
    assert len(payload) < 255
    rbsp = bytes([5, len(payload)]) + payload + b"\x80"
    return b"\x00\x00\x00\x01" + bytes([0x06]) + ep_add(rbsp)


def insert_sei(au: bytes, text: str) -> bytes:
    """Insert a SEAZN SEI NAL just before the first VCL NAL (type 1 or 5) of an access unit."""
    for pos, nal in nal_units(au):
        if nal and (nal[0] & 0x1F) in (1, 5):
            if pos > 0 and au[pos - 1] == 0:
                pos -= 1  # include the 4th byte of a 4-byte start code
            return au[:pos] + make_sei(text) + au[pos:]
    return au


def parse_seis(au: bytes):
    """Return a list of (uuid_bytes, text) for every user_data_unregistered SEI message in an AU,
    plus the NAL type list (for diagnostics)."""
    found, types = [], []
    for _pos, nal in nal_units(au):
        if not nal:
            continue
        t = nal[0] & 0x1F
        types.append(t)
        if t != 6:
            continue
        rb = ep_remove(nal[1:])
        i = 0
        while i < len(rb) and rb[i] != 0x80:
            pt = 0
            while i < len(rb) and rb[i] == 0xFF:
                pt += 255
                i += 1
            if i >= len(rb):
                break
            pt += rb[i]
            i += 1
            ps = 0
            while i < len(rb) and rb[i] == 0xFF:
                ps += 255
                i += 1
            if i >= len(rb):
                break
            ps += rb[i]
            i += 1
            body = rb[i:i + ps]
            i += ps
            if pt == 5 and len(body) >= 16:
                found.append((body[:16], body[16:]))
    return found, types
