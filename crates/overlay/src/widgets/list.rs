// Every widget, one line each: `module: Type = Spec::new(label, x, y)` with its default size, rate, and whether it
// is on. The module's name is the widget's `[widgets.<name>]` table in config.toml, and the order is the Pitot
// window's. New widgets go at the end and stay off unless their line ends in `.on()`; see "Adding a widget" in
// docs/overlay-rendering.md. Lines added on different branches merge without conflicts (.gitattributes).
// The default layout is for a 1920 x 1080 monitor: 8 px from its edges and 8 px between widgets, on a 4 px grid,
// with nothing overlapping (a test checks). Standings top left under the status, sectors and pit stops top center,
// the input trace bottom center, and a right column of track map, wind and tires, relative, and fuel.
widgets! {
    status: StatusWidget = Spec::new("Status", 8, 8).on(),
    inputs: InputTraceWidget = Spec::new("Input trace", 660, 952).size(600, 120).rate(60.0).on(),
    relative: RelativeWidget = Spec::new("Relative", 1456, 520).size(456, 396).rate(10.0).on(),
    standings: StandingsWidget = Spec::new("Standings", 8, 72).size(744, 560).rate(10.0).on(),
    fuel: FuelWidget = Spec::new("Fuel", 1456, 924).size(456, 148).rate(30.0).on(),
    pit: PitWidget = Spec::new("Pit stops", 760, 216).size(400, 280).rate(10.0).on(),
    sectors: SectorsWidget = Spec::new("Sectors", 760, 8).size(400, 200).rate(10.0).on(),
    wind: WindWidget = Spec::new("Wind", 1456, 344).size(224, 168).rate(10.0).on(),
    tires: TiresWidget = Spec::new("Tires", 1688, 344).size(224, 168).rate(10.0).on(),
    trackmap: TrackMapWidget = Spec::new("Track map", 1456, 8).size(456, 328).rate(10.0).on(),
}
