//! Consistent Overhead Byte Stuffing; frames on the USB serial link end with 0x00.

pub fn encode(src: &[u8], dst: &mut [u8]) -> usize {
    let mut code_idx = 0;
    let mut out = 1;
    let mut code = 1u8;
    for &b in src {
        if b == 0 {
            dst[code_idx] = code;
            code_idx = out;
            out += 1;
            code = 1;
        } else {
            dst[out] = b;
            out += 1;
            code += 1;
            if code == 0xFF {
                dst[code_idx] = code;
                code_idx = out;
                out += 1;
                code = 1;
            }
        }
    }
    dst[code_idx] = code;
    out
}

pub fn decode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut i = 0;
    let mut out = 0;
    while i < src.len() {
        let code = src[i] as usize;
        if code == 0 || i + code > src.len() {
            return None;
        }
        i += 1;
        for _ in 1..code {
            *dst.get_mut(out)? = src[i];
            out += 1;
            i += 1;
        }
        if code < 0xFF && i < src.len() {
            *dst.get_mut(out)? = 0;
            out += 1;
        }
    }
    Some(out)
}
