//! Whole-frame virtual primary-plane scaling. The host viewer performs sampling;
//! this path adds no guest staging surface or pixel transport.
use anyhow::{Context, Result, ensure};
use drm::control::{
    AtomicCommitFlags, Device, Mode, ResourceHandle, atomic::AtomicModeReq, connector, crtc,
    framebuffer, plane, property,
};

const PLANE_PROPERTIES: [&str; 10] = [
    "FB_ID", "CRTC_ID", "SRC_X", "SRC_Y", "SRC_W", "SRC_H", "CRTC_X", "CRTC_Y", "CRTC_W", "CRTC_H",
];

pub(crate) struct Plane {
    handle: plane::Handle,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    properties: [property::Handle; 10],
    connector_crtc: property::Handle,
    crtc_mode: property::Handle,
    crtc_active: property::Handle,
}

fn property(
    card: &impl Device,
    object: impl ResourceHandle,
    name: &str,
) -> Result<property::Handle> {
    for (handle, _) in card.get_properties(object)?.iter() {
        if card.get_property(*handle)?.name().to_bytes() == name.as_bytes() {
            return Ok(*handle);
        }
    }
    anyhow::bail!("virtual display is missing atomic property {name}")
}

impl Plane {
    pub(crate) fn new(
        card: &impl Device,
        handle: plane::Handle,
        crtc: crtc::Handle,
        connector: connector::Handle,
        mode: Mode,
    ) -> Result<Self> {
        let properties = PLANE_PROPERTIES
            .map(|name| property(card, handle, name))
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .try_into()
            .map_err(|_| anyhow::anyhow!("incomplete primary-plane properties"))?;
        Ok(Self {
            handle,
            crtc,
            connector,
            mode,
            properties,
            connector_crtc: property(card, connector, "CRTC_ID")?,
            crtc_mode: property(card, crtc, "MODE_ID")?,
            crtc_active: property(card, crtc, "ACTIVE")?,
        })
    }

    pub(crate) fn commit(
        &self,
        card: &impl Device,
        fb: framebuffer::Handle,
        source: (u32, u32),
        initial: bool,
        verify: bool,
    ) -> Result<u64> {
        let (crtc, connector, mode) = (self.crtc, self.connector, self.mode);
        let mut request = AtomicModeReq::new();
        let values = [
            u64::from(u32::from(fb)),
            u64::from(u32::from(crtc)),
            0,
            0,
            u64::from(source.0) << 16,
            u64::from(source.1) << 16,
            0,
            0,
            u64::from(mode.size().0),
            u64::from(mode.size().1),
        ];
        for (prop, value) in self.properties.into_iter().zip(values) {
            request.add_raw_property(self.handle.into(), prop, value);
        }
        let blob = if initial {
            let blob = card.create_property_blob(&mode)?;
            request.add_property(
                connector,
                self.connector_crtc,
                property::Value::CRTC(Some(crtc)),
            );
            request.add_property(crtc, self.crtc_mode, blob);
            request.add_property(crtc, self.crtc_active, property::Value::Boolean(true));
            Some(u64::from(blob))
        } else {
            None
        };
        // The caller retains both old and new buffers until the actual flip
        // event. NONBLOCK is submission, never proof of GPU read completion.
        let flags = AtomicCommitFlags::NONBLOCK
            | AtomicCommitFlags::PAGE_FLIP_EVENT
            | if initial {
                AtomicCommitFlags::ALLOW_MODESET
            } else {
                AtomicCommitFlags::empty()
            };
        let result = (|| -> Result<u64> {
            if verify {
                let test_flags =
                    (flags & AtomicCommitFlags::ALLOW_MODESET) | AtomicCommitFlags::TEST_ONLY;
                card.atomic_commit(test_flags, request.clone())
                    .context("valid whole-image scaling test")?;
                for (index, value) in [
                    (4, u64::from(source.0 - 1) << 16),
                    (8, u64::from(mode.size().0 - 1)),
                    (4, u64::from(source.0 + 1) << 16),
                ] {
                    let mut invalid = request.clone();
                    invalid.add_raw_property(self.handle.into(), self.properties[index], value);
                    let error = card
                        .atomic_commit(test_flags, invalid)
                        .err()
                        .context("virtual display accepted an unsupported partial image")?;
                    eprintln!(
                        "LEGION_KMS_SCALE_NEGATIVE property={} value={} errno={:?}",
                        PLANE_PROPERTIES[index],
                        value,
                        error.raw_os_error()
                    );
                    ensure!(
                        error.raw_os_error().is_some_and(|code| [
                            libc::EINVAL,
                            libc::ENOSPC,
                            libc::ERANGE
                        ]
                        .contains(&code)),
                        "invalid primary geometry failed unexpectedly: {error}"
                    );
                }
            }
            card.atomic_commit(flags, request)
                .context("submit whole-image virtual primary-plane transition")?;
            Ok(if verify { 3 } else { 0 })
        })();
        if let Some(blob) = blob {
            let _ = card.destroy_property_blob(blob);
        }
        result
    }
}
