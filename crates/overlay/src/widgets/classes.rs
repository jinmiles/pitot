//! Class labels: the session's class short name (`CarClassShortName`), or, where the session leaves it empty (AI
//! sessions do), a label worked out from the class ID or from the class's cars. The session info has no long class
//! name.

use pitot_irsdk::session::Driver;

/// Classes known by ID, for sessions that leave their short name empty. Only classes seen in captures with an empty
/// short name are listed; any other class is labeled from its cars ([`CAR_MARKERS`]).
const KNOWN_CLASSES: &[(i64, &str)] = &[
    // IMSA's top prototypes: the LMDh cars and the Ferrari 499P.
    (4029, "GTP"),
    // The Dallara P217.
    (2523, "LMP2"),
    // GT3 cars of every make.
    (4011, "GT3"),
];

/// Parts of a car's `CarPath` or `CarScreenName` that name the class it races in, case-insensitive, first match
/// first: `bmwlmdh` and `porsche963gtp` are GTP, `dallarap217` is LMP2, `bmwm4gt3` is GT3. Car names that carry
/// no class word, such as `ferrari499p` (an LMH car that races with the LMDh cars) and the Ligier JS P320, are
/// listed by name.
const CAR_MARKERS: &[(&str, &str)] = &[
    ("gtp", "GTP"),
    ("lmdh", "GTP"),
    ("499p", "GTP"),
    ("lmp2", "LMP2"),
    ("p217", "LMP2"),
    ("lmp3", "LMP3"),
    ("p320", "LMP3"),
    ("gt3", "GT3"),
    ("gt4", "GT4"),
    ("tcr", "TCR"),
];

/// Whether `text` contains `word`, ignoring ASCII case, without allocating.
fn contains(text: &str, word: &str) -> bool {
    text.as_bytes()
        .windows(word.len())
        .any(|w| w.eq_ignore_ascii_case(word.as_bytes()))
}

/// The class a car's names point to. One-make cup cars (the Porsche 911 GT3 Cup) carry a class word in their name
/// but race in a class of their own, so a name with `cup` in it points nowhere.
fn car_class(driver: &Driver) -> Option<&'static str> {
    let names = [
        driver.car_path.as_deref(),
        driver.car_screen_name.as_deref(),
    ];
    let names = names.iter().flatten();
    if names.clone().any(|n| contains(n, "cup")) {
        return None;
    }
    names
        .flat_map(|name| {
            CAR_MARKERS
                .iter()
                .find(|(word, _)| contains(name, word))
                .map(|&(_, label)| label)
        })
        .next()
}

/// The label of the class `driver` races in, among `drivers` (the session's, by `CarIdx`):
///
/// 1. the class short name, when the session gives one;
/// 2. else the label of a class known by its ID ([`KNOWN_CLASSES`]);
/// 3. else the class the class's cars point to by their names, when every car agrees ([`CAR_MARKERS`]);
/// 4. else the car's short name, as before there were class labels.
pub(crate) fn class_label<'a>(drivers: &'a [Option<Driver>], driver: &'a Driver) -> &'a str {
    if let Some(name) = driver
        .car_class_short_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        return name;
    }
    let class = driver.car_class_id;
    if let Some(&(_, label)) = class.and_then(|id| KNOWN_CLASSES.iter().find(|(k, _)| *k == id)) {
        return label;
    }
    let mut agreed = None;
    for car in drivers
        .iter()
        .flatten()
        .filter(|d| !d.is_pace_car && !d.is_spectator && d.car_class_id == class)
    {
        match (car_class(car), agreed) {
            (Some(label), None) => agreed = Some(label),
            (Some(label), Some(so_far)) if label == so_far => {}
            _ => {
                agreed = None;
                break;
            }
        }
    }
    agreed
        .or(driver.car_screen_name_short.as_deref())
        .unwrap_or("Class")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(idx: usize, class: i64, path: &str, screen: &str) -> Option<Driver> {
        Some(Driver {
            car_idx: idx,
            car_path: Some(path.into()),
            car_screen_name: Some(screen.into()),
            car_screen_name_short: Some(screen.into()),
            car_class_id: Some(class),
            ..Driver::default()
        })
    }

    fn label_of(drivers: &[Option<Driver>], idx: usize) -> &str {
        class_label(drivers, drivers[idx].as_ref().expect("driver"))
    }

    #[test]
    fn known_classes_with_no_short_name_get_their_label() {
        // The AI race capture's classes: every short name empty.
        let drivers = [
            car(0, 4029, "bmwlmdh", "BMW M Hybrid V8 (EVO)"),
            car(1, 2523, "dallarap217", "Dallara P217 LMP2"),
            car(2, 4011, "bmwm4gt3", "BMW M4 GT3 EVO"),
        ];
        assert_eq!(label_of(&drivers, 0), "GTP");
        assert_eq!(label_of(&drivers, 1), "LMP2");
        assert_eq!(label_of(&drivers, 2), "GT3");
    }

    #[test]
    fn a_short_name_is_used_as_it_is() {
        let mut drivers = [
            car(0, 3188, "formulair04", "FIA F4"),
            car(1, 4011, "bmwm4gt3", "BMW M4 GT3"),
        ];
        for (d, name) in drivers.iter_mut().flatten().zip(["FIA F4", " GT3 Pro "]) {
            d.car_class_short_name = Some(name.into());
        }
        assert_eq!(label_of(&drivers, 0), "FIA F4");
        assert_eq!(
            label_of(&drivers, 1),
            "GT3 Pro",
            "trimmed, and before the known ID"
        );
    }

    #[test]
    fn unknown_classes_are_labeled_by_their_cars_when_all_agree() {
        let drivers = [
            car(0, 1, "porsche963gtp", "Porsche 963 GTP"),
            car(1, 1, "ferrari499p", "Ferrari 499P"),
            car(2, 1, "cadillacvseriesrgtp", "Cadillac V-Series.R"),
            car(3, 2, "dallarap217", "Dallara P217 LMP2"),
            car(4, 3, "ligierjsp320", "Ligier JS P320"),
            car(5, 4, "porsche718gt4", "Porsche 718 Cayman GT4 Clubsport MR"),
            car(6, 4, "bmwm4gt4", "BMW M4 GT4"),
            car(7, 5, "hyundaielantran", "Hyundai Elantra N TCR"),
            car(8, 6, "acuransxevo22gt3", "Acura NSX GT3 EVO 22"),
            car(9, 6, "chevyvettez06rgt3", "Chevrolet Corvette Z06 GT3.R"),
            car(10, 7, "bmwlmdh", "BMW M Hybrid V8"),
        ];
        let labels: Vec<&str> = (0..drivers.len()).map(|i| label_of(&drivers, i)).collect();
        assert_eq!(
            labels,
            [
                "GTP", "GTP", "GTP", "LMP2", "LMP3", "GT4", "GT4", "TCR", "GT3", "GT3", "GTP"
            ]
        );
    }

    #[test]
    fn classes_whose_cars_disagree_keep_the_car_name() {
        let mut drivers = vec![
            car(0, 1, "bmwm4gt3", "BMW M4 GT3"),
            car(1, 1, "bmwm4gt4", "BMW M4 GT4"),
            // A car with no class word in its names.
            car(2, 2, "mx5 mx52016", "Global Mazda MX-5 Cup"),
            car(3, 3, "porsche992cup", "Porsche 911 GT3 Cup (992)"),
            car(4, 4, "bmwm4gt3", "BMW M4 GT3"),
            car(5, 4, "radical sr8", "Radical SR8"),
        ];
        assert_eq!(label_of(&drivers, 0), "BMW M4 GT3");
        assert_eq!(label_of(&drivers, 2), "Global Mazda MX-5 Cup");
        assert_eq!(
            label_of(&drivers, 3),
            "Porsche 911 GT3 Cup (992)",
            "a cup car is not GT3"
        );
        assert_eq!(label_of(&drivers, 4), "BMW M4 GT3");
        // The pace car and spectators do not count.
        drivers[5] = Some(Driver {
            is_pace_car: true,
            ..drivers[5].clone().expect("car")
        });
        assert_eq!(label_of(&drivers, 4), "GT3");
        // Without any name, a placeholder.
        let bare = [Some(Driver {
            car_class_id: Some(9),
            ..Driver::default()
        })];
        assert_eq!(label_of(&bare, 0), "Class");
    }
}
