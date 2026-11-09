//! The network profiles and the run sizes each one starts from.

use clap::ValueEnum;

/// A named network condition. The names are the ones irsync parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Profile {
    #[value(name = "4g-vehicle")]
    FourGVehicle,
    #[value(name = "dsl-video")]
    DslVideo,
    #[value(name = "test-blackout")]
    TestBlackout,
    #[value(name = "long-rtt")]
    LongRtt,
}

/// What a profile uses for anything the user did not set.
#[derive(Clone, Copy, Debug)]
pub struct Defaults {
    pub iterations: u64,
    pub steps: u64,
    pub max_total_size: &'static str,
    pub max_file_size: &'static str,
    /// Passed to the client as `--timeout`.
    pub client_timeout_secs: u64,
    /// Whether the profile has dead zones.
    pub has_outages: bool,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::FourGVehicle => "4g-vehicle",
            Profile::DslVideo => "dsl-video",
            Profile::TestBlackout => "test-blackout",
            Profile::LongRtt => "long-rtt",
        }
    }

    pub fn defaults(self) -> Defaults {
        match self {
            Profile::FourGVehicle => Defaults {
                iterations: 10,
                steps: 100,
                max_total_size: "256M",
                max_file_size: "32M",
                client_timeout_secs: 10,
                has_outages: true,
            },
            Profile::DslVideo => Defaults {
                iterations: 10,
                steps: 100,
                max_total_size: "32M",
                max_file_size: "4M",
                client_timeout_secs: 10,
                has_outages: false,
            },
            Profile::TestBlackout => Defaults {
                iterations: 3,
                steps: 50,
                max_total_size: "8M",
                max_file_size: "1M",
                client_timeout_secs: 2,
                has_outages: true,
            },
            // Many small files: where waiting for ACKs costs the most.
            Profile::LongRtt => Defaults {
                iterations: 5,
                steps: 2000,
                max_total_size: "64M",
                max_file_size: "64K",
                client_timeout_secs: 10,
                has_outages: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_names_match_irsync() {
        assert_eq!(Profile::FourGVehicle.name(), "4g-vehicle");
        assert_eq!(Profile::DslVideo.name(), "dsl-video");
        assert_eq!(Profile::TestBlackout.name(), "test-blackout");
        assert_eq!(Profile::LongRtt.name(), "long-rtt");
    }
}
