use crate::config::Levels;

pub fn warmth_to_kelvin(warmth: u8) -> u16 {
    (6500.0 - (warmth.min(100) as f64 / 100.0) * 4000.0).round() as u16
}

pub fn gamma_ramps(size: u32, levels: Levels) -> Vec<u16> {
    let size = size as usize;
    if size == 0 {
        return Vec::new();
    }
    let rgb = temperature_rgb(warmth_to_kelvin(levels.warmth));
    let brightness = levels.brightness as f64 / 100.0;
    let mut ramps = Vec::with_capacity(size * 3);
    for multiplier in rgb {
        for index in 0..size {
            let input = if size == 1 {
                1.0
            } else {
                index as f64 / (size - 1) as f64
            };
            let value = (u16::MAX as f64 * input * multiplier * brightness)
                .round()
                .clamp(0.0, u16::MAX as f64) as u16;
            ramps.push(value);
        }
    }
    ramps
}

// A compact approximation of black-body RGB for display white points.
fn temperature_rgb(kelvin: u16) -> [f64; 3] {
    if kelvin >= 6500 {
        return [1.0, 1.0, 1.0];
    }
    let temp = kelvin as f64 / 100.0;
    let red = if temp <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (temp - 60.0).powf(-0.133_204_759_2)
    };
    let green = if temp <= 66.0 {
        99.470_802_586_1 * temp.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (temp - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if temp >= 66.0 {
        255.0
    } else if temp <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (temp - 10.0).ln() - 305.044_792_730_7
    };
    [red, green, blue].map(|value| value.clamp(0.0, 255.0) / 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_warmth_endpoints() {
        assert_eq!(warmth_to_kelvin(0), 6500);
        assert_eq!(warmth_to_kelvin(50), 4500);
        assert_eq!(warmth_to_kelvin(100), 2500);
    }

    #[test]
    fn ramps_are_sized_bounded_and_monotonic() {
        let ramps = gamma_ramps(
            256,
            Levels {
                warmth: 70,
                brightness: 80,
            },
        );
        assert_eq!(ramps.len(), 768);
        for channel in ramps.chunks(256) {
            assert_eq!(channel[0], 0);
            assert!(channel.windows(2).all(|pair| pair[0] <= pair[1]));
        }
    }

    #[test]
    fn neutral_ramp_reaches_nearly_full_white() {
        let ramps = gamma_ramps(2, Levels::NEUTRAL);
        assert!(ramps[1] > 64_000);
        assert!(ramps[3] > 64_000);
        assert_eq!(ramps[5], u16::MAX);
    }

    #[test]
    fn warm_ramp_reduces_green_and_blue() {
        let ramps = gamma_ramps(
            2,
            Levels {
                warmth: 100,
                brightness: 100,
            },
        );
        let [red, green, blue] = [ramps[1], ramps[3], ramps[5]];
        assert!(red > green);
        assert!(green > blue);
    }
}
