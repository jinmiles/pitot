//! Class labels: the session's class short name (`CarClassShortName`), or, where the session leaves it empty (AI
//! sessions do), a label worked out from the class's cars. The session info has no long class name.
//!
//! The order of the rules follows irDashies: the short name, else the category every car of the class agrees on,
//! else the car's name when the class has one car model, else the class number. The category is a car's, not a
//! class's: class IDs are made per series, while a car keeps its `CarPath` everywhere.

use std::fmt;

use pitot_irsdk::session::Driver;

/// Car models whose `CarPath` does not say their category, by exact path.
const CAR_PATHS: &[(&str, &str)] = &[
    // An LMH car, run with the LMDh cars in the GTP class.
    ("ferrari499p", "GTP"),
    ("bmwlmdh", "GTP"),
    ("dallarap217", "LMP2"),
];

/// Parts of a `CarPath` that name the car's category, first match first: `porsche963gtp` is GTP, `bmwm4gt3` is
/// GT3. A path with `cup` in it is a one-make cup car (the Porsche 911 GT3 Cup) and has no category.
const PATH_MARKERS: &[(&str, &str)] = &[
    ("gtp", "GTP"),
    ("lmdh", "GTP"),
    ("lmp2", "LMP2"),
    ("lmp3", "LMP3"),
    ("gt3", "GT3"),
    ("gt4", "GT4"),
    ("tcr", "TCR"),
];

/// A class's label, written without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassLabel<'a> {
    Name(&'a str),
    /// `Class 4011`, or `Class` without an ID.
    Number(Option<i64>),
}

impl fmt::Display for ClassLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Number(Some(id)) => write!(f, "Class {id}"),
            Self::Number(None) => f.write_str("Class"),
        }
    }
}

/// Whether `text` contains `word`, ignoring ASCII case, without allocating.
fn contains(text: &str, word: &str) -> bool {
    text.as_bytes()
        .windows(word.len())
        .any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
}

/// The category of a car model by its `CarPath`.
fn car_category(path: &str) -> Option<&'static str> {
    if let Some(&(_, label)) = CAR_PATHS.iter().find(|(p, _)| p.eq_ignore_ascii_case(path)) {
        return Some(label);
    }
    if contains(path, "cup") {
        return None;
    }
    PATH_MARKERS
        .iter()
        .find(|(word, _)| contains(path, word))
        .map(|&(_, label)| label)
}

/// The label of the class `driver` races in, among `drivers` (the session's, by `CarIdx`; the pace car and
/// spectators do not count):
///
/// 1. the first `CarClassShortName` of the class that is not empty;
/// 2. else the category every car of the class has by its `CarPath` ([`CAR_PATHS`], [`PATH_MARKERS`]);
/// 3. else, when the class has one car model, its `CarScreenNameShort`;
/// 4. else `Class` and the class ID.
pub(crate) fn class_label<'a>(drivers: &'a [Option<Driver>], driver: &'a Driver) -> ClassLabel<'a> {
    let class = driver.car_class_id;
    let cars = || {
        drivers
            .iter()
            .flatten()
            .filter(move |d| !d.is_pace_car && !d.is_spectator && d.car_class_id == class)
    };
    if let Some(name) = cars()
        .chain(std::iter::once(driver))
        .filter_map(|d| d.car_class_short_name.as_deref().map(str::trim))
        .find(|n| !n.is_empty())
    {
        return ClassLabel::Name(name);
    }
    let path = |d: &'a Driver| {
        d.car_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    };
    // The category all cars agree on, and the one car model of the class; a car without either spoils it.
    let (mut category, mut agreed) = (None, true);
    let (mut model, mut one_model) = (None, true);
    for car in cars() {
        let this = path(car);
        match (this.and_then(car_category), category) {
            (Some(c), None) => category = Some(c),
            (Some(c), Some(so_far)) if c == so_far => {}
            _ => agreed = false,
        }
        match (this, model) {
            (Some(p), None) => model = Some(p),
            (Some(p), Some(m)) if p.eq_ignore_ascii_case(m) => {}
            _ => one_model = false,
        }
    }
    if let Some(category) = category.filter(|_| agreed) {
        return ClassLabel::Name(category);
    }
    if one_model
        && let Some(name) = driver
            .car_screen_name_short
            .as_deref()
            .or(driver.car_screen_name.as_deref())
            .filter(|n| !n.trim().is_empty())
    {
        return ClassLabel::Name(name);
    }
    ClassLabel::Number(class)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(idx: usize, class: i64, path: &str, short: &str) -> Option<Driver> {
        Some(Driver {
            car_idx: idx,
            car_path: Some(path.into()),
            car_screen_name: Some(short.into()),
            car_screen_name_short: Some(short.into()),
            car_class_id: Some(class),
            ..Driver::default()
        })
    }

    fn label_of(drivers: &[Option<Driver>], idx: usize) -> String {
        class_label(drivers, drivers[idx].as_ref().expect("driver")).to_string()
    }

    #[test]
    fn imsa_classes_with_no_short_name_get_their_category() {
        // The AI race capture's classes, every short name empty.
        let drivers = [
            car(0, 4029, "bmwlmdh", "BMW M Hybrid V8 (EVO)"),
            car(1, 4029, "ferrari499p", "Ferrari 499P"),
            car(2, 4029, "porsche963gtp", "Porsche 963"),
            car(3, 4029, "acuraarx06gtp", "Acura ARX-06"),
            car(4, 4029, "cadillacvseriesrgtp", "Cadillac V-Series.R"),
            car(5, 2523, "dallarap217", "Dallara P217 LMP2"),
            car(6, 4011, "bmwm4gt3", "BMW M4 GT3 EVO"),
            car(7, 4011, "amvantageevogt3", "AMR Vantage GT3 EVO"),
            car(8, 4011, "chevyvettez06rgt3", "Corvette GT3.R"),
        ];
        assert_eq!(label_of(&drivers, 0), "GTP");
        assert_eq!(label_of(&drivers, 5), "LMP2");
        assert_eq!(label_of(&drivers, 6), "GT3");
    }

    #[test]
    fn the_category_follows_the_cars_not_the_class_id() {
        // The same cars in classes another series made.
        let drivers = [
            car(0, 77, "bmwm4gt3", "BMW M4 GT3"),
            car(1, 77, "porsche992rgt3", "Porsche 911 GT3 R (992)"),
            car(2, 78, "bmwm4gt4", "BMW M4 GT4"),
            car(3, 78, "porsche718gt4", "Porsche 718 Cayman GT4"),
            car(4, 79, "ligierjslmp3", "Ligier LMP3"),
            car(5, 80, "audirs3tcr", "Audi RS 3 LMS TCR"),
        ];
        let labels: Vec<String> = [0, 2, 4, 5].map(|i| label_of(&drivers, i)).into();
        assert_eq!(labels, ["GT3", "GT4", "LMP3", "TCR"]);
    }

    #[test]
    fn a_short_name_in_the_class_is_used_as_it_is() {
        let mut drivers = vec![
            car(0, 3188, "formulair04", "FIA F4"),
            car(1, 4011, "bmwm4gt3", "BMW M4 GT3"),
            car(2, 4011, "bmwm4gt3", "BMW M4 GT3"),
        ];
        if let Some(d) = drivers[0].as_mut() {
            d.car_class_short_name = Some("FIA F4".into());
        }
        // Another car of the class has it.
        if let Some(d) = drivers[2].as_mut() {
            d.car_class_short_name = Some(" GT3 Pro ".into());
        }
        assert_eq!(label_of(&drivers, 0), "FIA F4");
        assert_eq!(
            label_of(&drivers, 1),
            "GT3 Pro",
            "trimmed, before the cars' category"
        );
    }

    #[test]
    fn classes_whose_cars_disagree_show_one_model_or_the_class_number() {
        let mut drivers = vec![
            car(0, 1, "bmwm4gt3", "BMW M4 GT3"),
            car(1, 1, "bmwm4gt4", "BMW M4 GT4"),
            // One car model with no category: its name.
            car(2, 2, "mx5 mx52016", "Global Mazda MX-5 Cup"),
            car(3, 2, "mx5 mx52016", "Global Mazda MX-5 Cup"),
            // A cup car is not GT3.
            car(4, 3, "porsche992cup", "Porsche 911 GT3 Cup (992)"),
            car(5, 4, "bmwm4gt3", "BMW M4 GT3"),
            car(6, 4, "radical sr8", "Radical SR8"),
        ];
        assert_eq!(label_of(&drivers, 0), "Class 1");
        assert_eq!(label_of(&drivers, 2), "Global Mazda MX-5 Cup");
        assert_eq!(label_of(&drivers, 4), "Porsche 911 GT3 Cup (992)");
        assert_eq!(label_of(&drivers, 5), "Class 4");
        // The pace car and spectators do not count.
        drivers[6] = Some(Driver {
            is_pace_car: true,
            ..drivers[6].clone().expect("car")
        });
        assert_eq!(label_of(&drivers, 5), "GT3");
        // Without any name or ID.
        let bare = [Some(Driver::default())];
        assert_eq!(label_of(&bare, 0), "Class");
    }
}
