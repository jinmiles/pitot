//! The telemetry variables the overlay reads, registered once and shared by the data and render threads.

use pitot_irsdk::live::{VarId, VarSet};

macro_rules! vars {
    ($($field:ident => $name:literal,)*) => {
        /// Handles to every variable the widgets use. Unavailable variables read as `None`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Vars {
            $(pub $field: VarId,)*
        }

        impl Vars {
            /// Adds every variable to `set` and returns their handles.
            pub fn register(set: &mut VarSet) -> Self {
                Self { $($field: set.add($name),)* }
            }
        }
    };
}

vars! {
    session_time => "SessionTime",
    session_num => "SessionNum",
    session_state => "SessionState",
    session_flags => "SessionFlags",
    session_time_remain => "SessionTimeRemain",
    session_laps_remain => "SessionLapsRemainEx",
    is_on_track => "IsOnTrack",
    is_replay_playing => "IsReplayPlaying",
    replay_session_num => "ReplaySessionNum",
    replay_session_time => "ReplaySessionTime",
    replay_play_speed => "ReplayPlaySpeed",
    on_pit_road => "OnPitRoad",
    player_car_idx => "PlayerCarIdx",
    speed => "Speed",
    gear => "Gear",
    rpm => "RPM",
    throttle => "ThrottleRaw",
    brake => "BrakeRaw",
    clutch => "Clutch",
    steering => "SteeringWheelAngle",
    abs_active => "BrakeABSactive",
    lap => "Lap",
    lap_completed => "LapCompleted",
    lap_dist_pct => "LapDistPct",
    lap_current_time => "LapCurrentLapTime",
    lap_last_time => "LapLastLapTime",
    lap_best_time => "LapBestLapTime",
    fuel_level => "FuelLevel",
    fuel_level_pct => "FuelLevelPct",
    fuel_use_per_hour => "FuelUsePerHour",
    car_lap_dist_pct => "CarIdxLapDistPct",
    car_est_time => "CarIdxEstTime",
    car_f2_time => "CarIdxF2Time",
    car_position => "CarIdxPosition",
    car_class_position => "CarIdxClassPosition",
    car_lap => "CarIdxLap",
    car_lap_completed => "CarIdxLapCompleted",
    car_on_pit_road => "CarIdxOnPitRoad",
    car_track_surface => "CarIdxTrackSurface",
    car_last_lap_time => "CarIdxLastLapTime",
    car_best_lap_time => "CarIdxBestLapTime",
    car_class => "CarIdxClass",
    car_tire_compound => "CarIdxTireCompound",
    air_temp => "AirTemp",
    track_temp => "TrackTempCrew",
    water_temp => "WaterTemp",
    incidents => "PlayerCarMyIncidentCount",
    time_of_day => "SessionTimeOfDay",
    car_session_flags => "CarIdxSessionFlags",
    wind_vel => "WindVel",
    wind_dir => "WindDir",
    yaw_north => "YawNorth",
    // The tire widget: wear and carcass temperatures (updated in the pit stall), distance on each tire.
    tire_compound => "PlayerTireCompound",
    tire_set_limit => "PlayerCarDryTireSetLimit",
    tire_sets_used => "TireSetsUsed",
    lf_wear_l => "LFwearL",
    lf_wear_m => "LFwearM",
    lf_wear_r => "LFwearR",
    lf_carcass_l => "LFtempCL",
    lf_carcass_m => "LFtempCM",
    lf_carcass_r => "LFtempCR",
    lf_odometer => "LFodometer",
    rf_wear_l => "RFwearL",
    rf_wear_m => "RFwearM",
    rf_wear_r => "RFwearR",
    rf_carcass_l => "RFtempCL",
    rf_carcass_m => "RFtempCM",
    rf_carcass_r => "RFtempCR",
    rf_odometer => "RFodometer",
    lr_wear_l => "LRwearL",
    lr_wear_m => "LRwearM",
    lr_wear_r => "LRwearR",
    lr_carcass_l => "LRtempCL",
    lr_carcass_m => "LRtempCM",
    lr_carcass_r => "LRtempCR",
    lr_odometer => "LRodometer",
    rr_wear_l => "RRwearL",
    rr_wear_m => "RRwearM",
    rr_wear_r => "RRwearR",
    rr_carcass_l => "RRtempCL",
    rr_carcass_m => "RRtempCM",
    rr_carcass_r => "RRtempCR",
    rr_odometer => "RRodometer",
}
