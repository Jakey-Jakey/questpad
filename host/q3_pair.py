import hashlib
import json
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from bridge import Bridge
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey, X25519PublicKey
from cryptography.hazmat.primitives.ciphers.aead import AESCCM


def x25519_keygen():
    priv = X25519PrivateKey.generate()
    pub = priv.public_key().public_bytes_raw()
    return priv.private_bytes_raw(), pub


def x25519_derive(priv_bytes, peer_pub_bytes):
    priv_key = X25519PrivateKey.from_private_bytes(priv_bytes)
    peer_key = X25519PublicKey.from_public_bytes(peer_pub_bytes)
    return priv_key.exchange(peer_key)


def aes_ccm_encrypt(key, iv_8, plaintext, aad=b"\x00", tag_len=4):
    nonce_13 = b"\x00" * 5 + iv_8
    aesccm = AESCCM(key, tag_length=tag_len)
    return aesccm.encrypt(nonce_13, plaintext, aad if aad else None)


def pair_q3(b, network_id, link_key):
    print("Initiating Quest 3 ECDH pairing...")
    # Step 1: Generate Host X25519 keypair
    host_priv, host_pub = x25519_keygen()
    print(f"Host X25519 public key: {host_pub.hex()}")

    # Step 2: Send Command 0x12 (ecdh_public_key) with READ flag (read=True)
    # The payload is our 32-byte public key; controller returns its 32-byte public key
    print("Exchanging ECDH public keys (cmd 0x12, read=True)...")
    err, ctrl_pub = b.dm_cmd(0x12, payload=host_pub, read=True, timeout=3.0)
    if err or len(ctrl_pub) < 32:
        raise RuntimeError(f"ECDH public key exchange failed: err={err}, len={len(ctrl_pub)}, resp={ctrl_pub.hex() if ctrl_pub else 'empty'}")
    ctrl_pub = ctrl_pub[:32]
    print(f"Controller X25519 public key: {ctrl_pub.hex()}")

    # Step 3: Compute shared secret
    shared_secret = x25519_derive(host_priv, ctrl_pub)
    print(f"Shared secret established (fingerprint: {hashlib.sha256(shared_secret).hexdigest()[:8]})")
    aes_key = shared_secret[:16]

    # Step 4: Encrypt pairing record with AES-128-CCM (20 bytes plain + 4 bytes MAC = 24 bytes)
    plaintext = struct.pack("<I", network_id) + link_key
    
    # Try with AAD = b"\x00" (Nordic S0 header) first, fallback to empty AAD if needed
    for aad in [b"\x00", b""]:
        iv_8 = os.urandom(8)
        ct_with_tag = aes_ccm_encrypt(aes_key, iv_8, plaintext, aad=aad, tag_len=4)
        payload = iv_8 + ct_with_tag  # 32 bytes (8B IV + 20B ciphertext + 4B MAC)
        print(f"Encrypted pairing payload ({len(payload)}B, aad={aad!r}): {payload.hex()}")

        # Step 5: Send Command 0x11 (ecdh_pairing without per-device key)
        # Using 0x11 ensures the controller stores our plaintext link_key (plaintext + 4) directly
        print("Writing encrypted pairing info (cmd 0x11, read=False)...")
        err, resp = b.dm_cmd(0x11, payload=payload, read=False, timeout=3.0)
        if not err:
            print("SUCCESS: Controller accepted encrypted pairing record via 0x11!")
            return True
        print(f"Command 0x11 rejected (err={err}, resp={resp.hex() if resp else 'empty'}), trying fallback Command 0x1D...")
        err, resp = b.dm_cmd(0x1D, payload=payload, read=False, timeout=3.0)
        if not err:
            print("SUCCESS: Controller accepted encrypted pairing record via 0x1D!")
            return True

    raise RuntimeError("Controller rejected encrypted pairing info with both 0x1D and 0x11")


def main():
    b = Bridge()
    print("dongle:", b.ping())
    b.dm_start(any_device=True)
    print("Waiting for controller in pairing mode (hold Oculus + B / Oculus + Y)...")
    connected, id0, id1 = b.dm_wait_link(timeout=120)
    print(f"Connected: id0={id0:#010x} id1={id1:#010x}")

    from dm import load_or_create_pairing, PAIRING_FILE, RESET
    p = load_or_create_pairing()
    net_id = p["network_id"]
    link_key = bytes.fromhex(p["link_key"])

    pair_q3(b, net_id, link_key)

    p["controllers"][f"{id0:08x}{id1:08x}"] = {"paired_at": time.strftime("%Y-%m-%d %H:%M:%S")}
    json.dump(p, open(PAIRING_FILE, "w"), indent=2)
    print(f"Pairing stored in {PAIRING_FILE}: network {net_id:#010x}")

    try:
        b.dm_cmd(RESET)
    except Exception:
        pass
    print("Controller reset into main app")
    stored = b.pair_store(net_id, link_key)
    print("Pairing saved to dongle flash" if stored else "WARNING: Failed to save to dongle flash")
    b.link_resume()
    print("Hosting link resumed!")


if __name__ == "__main__":
    main()
