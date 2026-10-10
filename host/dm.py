"""Talk to a controller in DM/pairing mode (hold HOME+Y or HOME+B first).

  dm.py info                 connect to the first advertising controller, print serial/versions/battery
  dm.py pair [--key HEX]     write the dongle's pairing record (network id + link key), then reset
"""
import argparse
import json
import os
import secrets
import struct
import time

from bridge import Bridge

PAIRING_FILE = os.path.join(os.path.dirname(__file__), "pairing.json")

GET_SN, GET_APP_VER, GET_SPL_VER, GET_BATTERY, IS_UNLOCKED = 0x03, 0x24, 0x25, 0x2F, 0x3D
SET_PAIRING, RESET = 0x14, 0x15


def good_network_id():
    """Random 32-bit radio address with no long runs of identical bits (Nordic guidance)."""
    while True:
        v = secrets.randbits(32)
        bits = f"{v:032b}"
        transitions = sum(bits[i] != bits[i + 1] for i in range(31))
        if transitions >= 10 and "000000" not in bits and "111111" not in bits and v not in (0xFACEB00C,):
            return v


def load_or_create_pairing():
    if os.path.exists(PAIRING_FILE):
        return json.load(open(PAIRING_FILE))
    p = {"network_id": good_network_id(), "link_key": secrets.token_hex(16), "controllers": {}}
    json.dump(p, open(PAIRING_FILE, "w"), indent=2)
    return p


def connect(b):
    b.dm_start(any_device=True)
    print("waiting for a controller in pairing mode (HOME+Y / HOME+B)...")
    connected, id0, id1 = b.dm_wait_link(timeout=90)
    print(f"connected: id0={id0:#010x} id1={id1:#010x}")
    time.sleep(0.05)
    return id0, id1


def info(b):
    err, sn = b.dm_cmd(GET_SN, read=True)
    print("serial:     ", sn.rstrip(b"\0").decode(errors="replace"), "(err)" if err else "")
    for name, cmd in [("app version", GET_APP_VER), ("spl version", GET_SPL_VER)]:
        err, v = b.dm_cmd(cmd, read=True)
        print(f"{name}: ", "err" if err else f"{v[0]}.{v[1]}.{v[2]}.{v[3]} {v[4:].rstrip(bytes(1)).decode(errors='replace')}  raw={v.hex()}")
    err, v = b.dm_cmd(GET_BATTERY, read=True)
    print("battery:    ", v.hex(), "(err)" if err else "")
    err, v = b.dm_cmd(IS_UNLOCKED, read=True)
    print("unlocked:   ", v.hex(), "(err)" if err else "")
    print("link stats: ", b.dm_stats())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("action", choices=["info", "pair"])
    args = ap.parse_args()
    b = Bridge()
    print("dongle:", b.ping())
    id0, id1 = connect(b)
    info(b)
    if args.action == "pair":
        p = load_or_create_pairing()
        rec = struct.pack("<I", p["network_id"]) + bytes.fromhex(p["link_key"])
        err, _ = b.dm_cmd(SET_PAIRING, rec)
        if err:
            raise SystemExit("controller rejected the pairing record")
        p["controllers"][f"{id0:08x}{id1:08x}"] = {"paired_at": time.strftime("%Y-%m-%d %H:%M:%S")}
        json.dump(p, open(PAIRING_FILE, "w"), indent=2)
        print(f"pairing written: network id {p['network_id']:#010x}")
        try:
            b.dm_cmd(RESET)
        except (TimeoutError, RuntimeError):
            pass  # the controller resets before/after acking
        print("controller reset into its main app")
        stored = b.pair_store(p["network_id"], bytes.fromhex(p["link_key"]))
        print("pairing saved to the dongle" if stored else "WARNING: saving the pairing to the dongle failed")
    b.link_resume()  # back to hosting the main link (if a pairing is stored)


if __name__ == "__main__":
    main()
