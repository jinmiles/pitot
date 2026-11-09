//! PDH GPU instance names and per-process engine load.
//!
//! `GPU Engine` instances look like `pid_1048_luid_0x00000000_0x0000F562_phys_0_eng_0_engtype_3D`, and
//! `GPU Process Memory` instances like `pid_1048_luid_0x00000000_0x0000F562_phys_0`.

/// The PID of a GPU counter instance.
pub fn parse_pid(instance: &str) -> Option<u32> {
    let rest = instance.strip_prefix("pid_")?;
    rest.get(..rest.find('_')?)?.parse().ok()
}

/// PID, adapter (`luid_...`), and engine type of a `GPU Engine` instance.
pub fn parse_engine(instance: &str) -> Option<(u32, &str, &str)> {
    let pid = parse_pid(instance)?;
    let adapter = instance.get(instance.find("luid_")?..instance.find("_phys_")?)?;
    let kind = instance.get(instance.find("engtype_")? + "engtype_".len()..)?;
    Some((pid, adapter, kind))
}

#[derive(Debug)]
struct Engine {
    adapter: String,
    kind: String,
    load: f64,
}

/// Utilization summed per adapter and engine type over a set of processes. Engines stay listed after
/// [`clear`](Self::clear), so steady sampling does not allocate.
#[derive(Debug, Default)]
pub struct EngineLoad {
    engines: Vec<Engine>,
}

impl EngineLoad {
    pub fn clear(&mut self) {
        self.engines.iter_mut().for_each(|e| e.load = 0.0);
    }

    pub fn add(&mut self, adapter: &str, kind: &str, load: f64) {
        match self
            .engines
            .iter_mut()
            .find(|e| e.adapter == adapter && e.kind.eq_ignore_ascii_case(kind))
        {
            Some(engine) => engine.load += load,
            None => self.engines.push(Engine {
                adapter: adapter.to_owned(),
                kind: kind.to_owned(),
                load,
            }),
        }
    }

    /// The busiest engine type on any adapter, as Task Manager shows a process's GPU use.
    pub fn busiest(&self) -> f64 {
        self.engines.iter().map(|e| e.load).fold(0.0, f64::max)
    }

    /// The load of one engine type, on the adapter where it is highest.
    pub fn of_kind(&self, kind: &str) -> f64 {
        self.engines
            .iter()
            .filter(|e| e.kind.eq_ignore_ascii_case(kind))
            .map(|e| e.load)
            .fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names() {
        let engine = "pid_1048_luid_0x00000000_0x0000f562_phys_0_eng_10_engtype_gdi render";
        assert_eq!(
            parse_engine(engine),
            Some((1048, "luid_0x00000000_0x0000f562", "gdi render"))
        );
        assert_eq!(
            parse_pid("pid_7_luid_0x00000000_0x0000F562_phys_0"),
            Some(7)
        );
        assert_eq!(parse_pid("_Total"), None);
        assert_eq!(parse_engine("pid_x_luid"), None);
    }

    #[test]
    fn load_per_engine_type() {
        let mut load = EngineLoad::default();
        load.add("luid_a", "3D", 2.0);
        load.add("luid_a", "3d", 1.5);
        load.add("luid_a", "Copy", 0.5);
        load.add("luid_b", "3D", 1.0);
        assert_eq!(load.of_kind("3D"), 3.5);
        assert_eq!(load.busiest(), 3.5);
        load.clear();
        assert_eq!(load.busiest(), 0.0);
        assert_eq!(
            load.engines.len(),
            3,
            "engines are kept for the next sample"
        );
    }
}
