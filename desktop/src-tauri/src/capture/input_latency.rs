//! Core Audio input-latency and transport lookup for the microphone lane.
//!
//! The HAL timestamp on an input callback marks when the samples left the
//! audio HAL, not when the sound reached the microphone. The difference is the
//! device's input pipeline latency (device latency + safety offset + stream
//! latency). Left uncompensated it lands in the recording as the mic running
//! late relative to video, most visibly on Bluetooth headsets. Same query and
//! clamp as Cap's `cap_audio::estimate_input_latency`.

use std::time::Duration;

use cidre::core_audio::{
    hardware::{Device, Stream, System},
    DeviceTransportType, PropElement, PropScope, PropSelector,
};

/// Cap on how much latency is subtracted from capture timestamps. Larger
/// values are almost certainly a misreported device property, and pulling
/// the mic that far ahead of video is worse than leaving it late.
const MAX_CAPTURE_LATENCY_COMPENSATION: Duration = Duration::from_millis(500);

/// Wireless input has encode + radio delay that Core Audio does not always
/// report; never compensate a wireless mic by less than this.
const WIRELESS_MIN_LATENCY: Duration = Duration::from_millis(120);

/// Capture buffer targets. Wireless transports deliver in bursts, so a
/// larger buffer avoids the queue starving between packets.
const WIRELESS_TARGET_LATENCY_MS: u32 = 80;
const WIRELESS_MIN_LATENCY_MS: u32 = 50;
const WIRELESS_MAX_LATENCY_MS: u32 = 200;
const ABS_MIN_BUFFER_FRAMES: u32 = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputTransport {
    Wired,
    Wireless,
    Airplay,
    ContinuityWireless,
    Unknown,
}

impl InputTransport {
    pub const fn is_wireless(self) -> bool {
        matches!(
            self,
            Self::Wireless | Self::Airplay | Self::ContinuityWireless
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct InputLatencyInfo {
    /// Latency between sound reaching the device and its HAL timestamp.
    pub capture_latency: Duration,
    pub transport: InputTransport,
}

impl InputLatencyInfo {
    pub const fn unknown() -> Self {
        Self {
            capture_latency: Duration::ZERO,
            transport: InputTransport::Unknown,
        }
    }
}

/// Resolve the device by its cpal name and read its input latency. Any
/// failure degrades to "no compensation", never to an error: latency is a
/// sync refinement and must not be able to fail a recording.
pub fn estimate_input_latency(device_name: &str) -> InputLatencyInfo {
    let Some(device) = input_device_by_name(device_name) else {
        return InputLatencyInfo::unknown();
    };
    compute_input_latency(&device).unwrap_or_else(|_| InputLatencyInfo::unknown())
}

fn input_device_by_name(target: &str) -> Option<Device> {
    System::devices().ok()?.into_iter().find(|device| {
        has_input_streams(device) && device.name().is_ok_and(|name| name.to_string() == target)
    })
}

fn has_input_streams(device: &Device) -> bool {
    device.streams().is_ok_and(|streams| {
        streams
            .iter()
            .any(|stream| is_input_stream(stream).unwrap_or(false))
    })
}

fn is_input_stream(stream: &Stream) -> cidre::os::Result<bool> {
    stream.direction().map(|direction| direction == 1)
}

fn compute_input_latency(device: &Device) -> cidre::os::Result<InputLatencyInfo> {
    let transport = transport_kind(
        device
            .transport_type()
            .unwrap_or(DeviceTransportType::UNKNOWN),
    );

    let device_latency_frames =
        scoped_u32(device, PropSelector::DEVICE_LATENCY, PropScope::INPUT).unwrap_or(0);
    let safety_offset_frames =
        scoped_u32(device, PropSelector::DEVICE_SAFETY_OFFSET, PropScope::INPUT).unwrap_or(0);
    let stream_latency_frames = max_input_stream_latency(device).unwrap_or(0);

    let device_rate = device.nominal_sample_rate().unwrap_or(0.0);
    if device_rate <= 0.0 {
        return Ok(InputLatencyInfo {
            capture_latency: transport_floor(transport, Duration::ZERO),
            transport,
        });
    }

    let total_frames =
        device_latency_frames as u64 + safety_offset_frames as u64 + stream_latency_frames as u64;
    let measured = Duration::from_secs_f64(total_frames as f64 / device_rate);
    Ok(InputLatencyInfo {
        capture_latency: transport_floor(transport, measured),
        transport,
    })
}

fn transport_floor(transport: InputTransport, measured: Duration) -> Duration {
    let floored = if transport.is_wireless() {
        measured.max(WIRELESS_MIN_LATENCY)
    } else {
        measured
    };
    floored.min(MAX_CAPTURE_LATENCY_COMPENSATION)
}

fn scoped_u32(device: &Device, selector: PropSelector, scope: PropScope) -> cidre::os::Result<u32> {
    device.prop(&selector.addr(scope, PropElement::MAIN))
}

fn max_input_stream_latency(device: &Device) -> cidre::os::Result<u32> {
    let mut max_latency = 0u32;
    for stream in device.streams()? {
        if is_input_stream(&stream)? {
            if let Ok(latency) = stream.latency() {
                max_latency = max_latency.max(latency);
            }
        }
    }
    Ok(max_latency)
}

fn transport_kind(transport: DeviceTransportType) -> InputTransport {
    match transport {
        DeviceTransportType::AIR_PLAY => InputTransport::Airplay,
        DeviceTransportType::BLUETOOTH | DeviceTransportType::BLUETOOTH_LE => {
            InputTransport::Wireless
        }
        DeviceTransportType::CONTINUITY_CAPTURE_WIRELESS => InputTransport::ContinuityWireless,
        DeviceTransportType::UNKNOWN => InputTransport::Unknown,
        _ => InputTransport::Wired,
    }
}

/// Capture buffer size for a wireless mic, clamped into the device's
/// supported range. `None` keeps the device default (wired transports).
pub fn wireless_buffer_frames(
    transport: InputTransport,
    sample_rate: u32,
    supported: &cpal::SupportedBufferSize,
) -> Option<u32> {
    if !transport.is_wireless() || sample_rate == 0 {
        return None;
    }
    let cpal::SupportedBufferSize::Range { min, max } = supported else {
        return None;
    };
    if *max == 0 {
        return None;
    }
    let frames = |ms: u32| ((sample_rate as u64 * ms as u64) / 1_000).max(1) as u32;
    let desired = frames(WIRELESS_TARGET_LATENCY_MS).clamp(
        frames(WIRELESS_MIN_LATENCY_MS),
        frames(WIRELESS_MAX_LATENCY_MS),
    );
    let device_min = ABS_MIN_BUFFER_FRAMES.min(*max).max(*min);
    Some(desired.clamp(device_min, *max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireless_floor_and_ceiling_bound_the_compensation() {
        assert_eq!(
            transport_floor(InputTransport::Wireless, Duration::from_millis(30)),
            WIRELESS_MIN_LATENCY
        );
        assert_eq!(
            transport_floor(InputTransport::Wired, Duration::from_millis(30)),
            Duration::from_millis(30)
        );
        assert_eq!(
            transport_floor(InputTransport::Wired, Duration::from_secs(4)),
            MAX_CAPTURE_LATENCY_COMPENSATION
        );
    }

    #[test]
    fn wireless_buffer_targets_80ms_inside_the_device_range() {
        let range = cpal::SupportedBufferSize::Range {
            min: 64,
            max: 4_096,
        };
        assert_eq!(
            wireless_buffer_frames(InputTransport::Wireless, 48_000, &range),
            Some(3_840)
        );
        assert_eq!(
            wireless_buffer_frames(InputTransport::Wired, 48_000, &range),
            None
        );
        let small = cpal::SupportedBufferSize::Range { min: 64, max: 512 };
        assert_eq!(
            wireless_buffer_frames(InputTransport::Wireless, 48_000, &small),
            Some(512)
        );
    }
}
