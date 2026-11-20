//! Decode the kernel's IN_FORMATS blob without relying on its alignment.
use anyhow::{Context, Result, ensure};
use smithay::backend::allocator::{Format, Fourcc, Modifier};

fn word<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    let end = offset
        .checked_add(N)
        .context("format blob offset overflow")?;
    Ok(data
        .get(offset..end)
        .context("truncated format blob")?
        .try_into()?)
}

pub(crate) fn decode(data: &[u8]) -> Result<Vec<Format>> {
    ensure!(
        u32::from_ne_bytes(word(data, 0)?) == 1,
        "unsupported format blob version"
    );
    let count = usize::try_from(u32::from_ne_bytes(word(data, 8)?))?;
    let codes = usize::try_from(u32::from_ne_bytes(word(data, 12)?))?;
    let modifiers = usize::try_from(u32::from_ne_bytes(word(data, 16)?))?;
    let records = usize::try_from(u32::from_ne_bytes(word(data, 20)?))?;
    ensure!(
        count <= 512 && modifiers <= 512,
        "format blob exceeds supported bounds"
    );
    let mut formats = Vec::new();
    for i in 0..modifiers {
        let at = records
            .checked_add(i * 24)
            .context("modifier offset overflow")?;
        let bits = u64::from_ne_bytes(word(data, at)?);
        let first = usize::try_from(u32::from_ne_bytes(word(data, at + 8)?))?;
        let modifier = Modifier::from(u64::from_ne_bytes(word(data, at + 16)?));
        for bit in 0..64 {
            if bits & (1_u64 << bit) == 0 {
                continue;
            }
            let index = first.checked_add(bit).context("format index overflow")?;
            ensure!(index < count, "modifier references an absent format");
            let at = codes
                .checked_add(index * 4)
                .context("format offset overflow")?;
            let Ok(code) = Fourcc::try_from(u32::from_ne_bytes(word(data, at)?)) else {
                continue;
            };
            if !matches!(
                code,
                Fourcc::Xrgb8888 | Fourcc::Argb8888 | Fourcc::Xbgr8888 | Fourcc::Abgr8888
            ) {
                continue;
            }
            let format = Format { code, modifier };
            if !formats.contains(&format) {
                formats.push(format);
            }
        }
    }
    Ok(formats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob() -> Vec<u8> {
        let mut data = Vec::new();
        for value in [1_u32, 0, 1, 24, 1, 28, Fourcc::Xrgb8888 as u32] {
            data.extend_from_slice(&value.to_ne_bytes());
        }
        data.extend_from_slice(&1_u64.to_ne_bytes());
        data.extend_from_slice(&0_u32.to_ne_bytes());
        data.extend_from_slice(&0_u32.to_ne_bytes());
        data.extend_from_slice(&0x0300_0000_0060_6014_u64.to_ne_bytes());
        data
    }

    #[test]
    fn preserves_tiled_modifier_and_rejects_malformed_references() {
        let mut data = blob();
        let formats = decode(&data).expect("valid unaligned modifier record");
        assert_eq!(formats.len(), 1);
        assert_eq!(u64::from(formats[0].modifier), 0x0300_0000_0060_6014);
        data[28..36].copy_from_slice(&2_u64.to_ne_bytes());
        assert!(decode(&data).is_err());
    }

    #[test]
    fn rejects_every_truncated_prefix() {
        let data = blob();
        for end in 0..data.len() {
            assert!(decode(&data[..end]).is_err());
        }
    }
}
