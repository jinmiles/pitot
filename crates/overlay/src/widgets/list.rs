// Every widget, one line each: `module: Type = Spec::new(label, x, y)` with its default size, rate, and whether it
// is on. The module's name is the widget's `[widgets.<name>]` table in config.toml, and the order is the Pitot
// window's. New widgets go at the end and stay off unless their line ends in `.on()`; see "Adding a widget" in
// docs/overlay-rendering.md. Lines added on different branches merge without conflicts (.gitattributes).
widgets! {
    status: StatusWidget = Spec::new("Status", 24, 24).on(),
    inputs: InputTraceWidget = Spec::new("Input trace", 660, 900).size(600, 120).rate(60.0).on(),
    relative: RelativeWidget = Spec::new("Relative", 1460, 560).size(460, 300).rate(10.0).on(),
    standings: StandingsWidget = Spec::new("Standings", 24, 260).size(760, 560).rate(10.0).on(),
    fuel: FuelWidget = Spec::new("Fuel", 1460, 900).size(420, 150).rate(30.0).on(),
    pit: PitWidget = Spec::new("Pit stops", 800, 240).size(400, 280).rate(10.0).on(),
    sectors: SectorsWidget = Spec::new("Sectors", 800, 24).size(400, 200).rate(10.0).on(),
    wind: WindWidget = Spec::new("Wind", 1460, 370).size(200, 170).rate(10.0).on(),
    tires: TiresWidget = Spec::new("Tires", 1680, 370).size(220, 170).rate(10.0).on(),
    trackmap: TrackMapWidget = Spec::new("Track map", 1460, 24).size(440, 330).rate(10.0).on(),
}
