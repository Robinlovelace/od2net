pub mod config;
pub mod detailed_route_output;
pub mod network;
pub mod node_map;
pub mod od;
pub mod plugins;
pub mod requests;
pub mod router;
pub mod timer;
pub mod utils;

pub use config::{CostFunction, GeneralizedCostFunction, InputConfig, LtsMapping, ODPattern, Requests, Uptake};
pub use network::{Counts, Network};
pub use requests::Request;

use std::io::BufWriter;
use std::process::Command;

use anyhow::{bail, Result};
use clap::Parser;
use fs_err::File;
use indicatif::HumanCount;
use instant::{Duration, Instant};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[clap(about, version, author)]
pub struct CliArgs {
    /// The path to a JSON file representing an InputConfig
    config_path: String,
    /// Specify a random number seed, used only for some generated request patterns, like BetweenZones.
    #[clap(long, default_value_t = 42)]
    rng_seed: u64,

    /// Don't output a CSV file with each edge's counts.
    #[clap(long)]
    no_output_csv: bool,
    /// Don't output a GeoJSON file with failed requests.
    #[clap(long)]
    no_output_failed_requests: bool,
    /// Don't output origin and destination points in the GeoJSON output, to reduce file size.
    #[clap(long)]
    no_output_od_points: bool,
    /// Don't output OSM tags in the GeoJSON output, to reduce file size.
    #[clap(long)]
    no_output_osm_tags: bool,
    /// Don't create a PMTiles file from the GeoJSON output. The results won't be viewable in the
    /// web app.
    #[clap(long)]
    no_output_pmtiles: bool,

    /// Create an `output/metadata.json` file summarizing the run.
    #[clap(long)]
    output_metadata: bool,

    // TODO These two should maybe be subcommands
    /// Instead of running normally, instead calculate this many routes and write a separate
    /// GeoJSON file for each of them, with full segment-level detail. This will be slow and take
    /// lots of disk if you specify a large number.
    #[clap(long)]
    detailed_routes: Option<usize>,

    /// Instead of running normally, just write a `network.geojson` with the OSM tags, LTS, and
    /// cost for every edge in a network. No counts are calculated or included.
    #[clap(long)]
    dump_network: bool,
}

/// Convenience helper so downstream callers can reuse the CLI interface or
/// construct `CliArgs` manually.
pub fn run_cli() -> Result<()> {
    let args = CliArgs::parse();
    run_pipeline(args)
}

pub fn run_pipeline(args: CliArgs) -> Result<()> {
    let config_json = fs_err::read_to_string(&args.config_path)?;
    let mut config: crate::config::InputConfig = match serde_json::from_str(&config_json) {
        Ok(config) => config,
        Err(err) => panic!("{} is invalid: {err}", args.config_path),
    };
    println!(
        "Using config from {}:\n{}\n",
        args.config_path,
        serde_json::to_string_pretty(&config)?
    );

    // Assume the config file is in the directory for the area
    let absolute_path = std::fs::canonicalize(&args.config_path).unwrap();
    let directory = absolute_path.parent().unwrap().display();
    fs_err::create_dir_all(format!("{directory}/intermediate"))?;
    fs_err::create_dir_all(format!("{directory}/output"))?;

    let mut timer = crate::timer::Timer::new();
    let pipeline_start = Instant::now();

    timer.start("Load network");
    let network = {
        let bin_path = format!("{directory}/intermediate/network.bin");
        let osm_pbf_path = format!("{directory}/input/input.osm.pbf");
        let osm_xml_path = format!("{directory}/input/input.osm.xml");
        println!("Trying to load network from {bin_path}");
        // TODO timer around something fallible is annoying
        match crate::network::Network::load_from_bin(&bin_path) {
            Ok(network) => network,
            Err(err) => {
                // The input is usually PBF, but could be XML
                let osm_path = if fs_err::metadata(&osm_pbf_path).is_ok() {
                    osm_pbf_path
                } else {
                    osm_xml_path
                };

                println!("That failed ({err}), so generating it from {osm_path}");
                let geotiff_bytes = if let Some(ref filename) = config.elevation_geotiff {
                    Some(fs_err::read(&format!("{directory}/input/{filename}"))?)
                } else {
                    None
                };

                let network = crate::network::Network::make_from_osm(
                    &fs_err::read(osm_path)?,
                    &config.lts,
                    &mut config.cost,
                    &mut timer,
                    geotiff_bytes,
                )?;

                timer.start(format!("Saving to {bin_path}"));
                let writer = BufWriter::new(File::create(bin_path)?);
                bincode::serialize_into(writer, &network)?;
                timer.stop();

                network
            }
        }
    };
    timer.stop();

    if args.dump_network {
        println!("Dumping network to network.geojson");
        fs_err::write("network.geojson", &network.to_debug_geojson()?)?;
        return Ok(());
    }

    timer.start("Loading or generating requests");
    let requests = crate::od::generate_requests(
        &config.requests,
        format!("{directory}/input"),
        &network,
        args.rng_seed,
        &mut timer,
    )?;
    let num_requests = requests.len();
    println!("Got {} requests", HumanCount(num_requests as u64));
    timer.stop();

    if let Some(num_routes) = args.detailed_routes {
        return crate::detailed_route_output::run(
            num_routes,
            &format!("{directory}/intermediate/ch.bin"),
            &network,
            requests,
            &config.uptake,
            format!("{directory}/output/"),
            &mut timer,
        );
    }

    timer.start("Routing");
    let routing_start = Instant::now();
    let counts = crate::router::run(
        &format!("{directory}/intermediate/ch.bin"),
        &network,
        requests,
        &config.uptake,
        &mut timer,
    )?;
    println!(
        "Got counts for {} edges",
        HumanCount(counts.count_per_edge.len() as u64),
    );
    println!(
        "{} succeeded, and {} failed",
        HumanCount(num_requests as u64 - counts.num_errors() as u64),
        HumanCount(counts.num_errors() as u64),
    );
    let routing_time = Instant::now().duration_since(routing_start);
    timer.stop();

    if !args.no_output_csv {
        timer.start("Writing per-edge output CSV");
        network.write_csv(&format!("{directory}/output/counts.csv"), &counts)?;
        timer.stop();

        timer.start("Writing per-OD output CSV");
        counts.write_od_csv(&format!("{directory}/output/od.csv"))?;
        timer.stop();
    }

    if !args.no_output_failed_requests {
        timer.start("Writing failed requests GJ");
        write_failed_requests(
            format!("{directory}/output/failed_requests.geojson"),
            &counts,
        )?;
        timer.stop();
    }

    let mut output_metadata =
        crate::OutputMetadata::new(config, &counts, num_requests, routing_time);
    timer.start("Writing output GJ");
    network.write_geojson(
        geojson::FeatureWriter::from_writer(std::io::BufWriter::new(fs_err::File::create(
            format!("{directory}/output/output.geojson"),
        )?)),
        counts,
        !args.no_output_od_points,
        !args.no_output_osm_tags,
        &output_metadata,
    )?;
    timer.stop();

    if !args.no_output_pmtiles {
        timer.start("Converting to pmtiles for rendering");
        let tippecanoe_start = Instant::now();
        let mut cmd = Command::new("tippecanoe");
        cmd.arg(format!("{directory}/output/output.geojson"))
            .arg("-o")
            .arg(format!("{directory}/output/rnet.pmtiles"))
            .arg("--force") // Overwrite existing output
            .arg("-l")
            .arg("rnet")
            .arg("-zg") // Guess the zoom
            .arg("--drop-fraction-as-needed") // TODO Drop based on low counts
            .arg("--extend-zooms-if-still-dropping")
            // Plumb through the config as a JSON string in the description
            .arg("--description")
            .arg(serde_json::to_string(&output_metadata)?);
        println!("Running: {cmd:?}");
        if !cmd.status()?.success() {
            bail!("tippecanoe failed");
        }
        output_metadata.tippecanoe_time_seconds = Some(
            Instant::now()
                .duration_since(tippecanoe_start)
                .as_secs_f32(),
        );
        timer.stop();
    }

    output_metadata.total_time_seconds =
        Some(Instant::now().duration_since(pipeline_start).as_secs_f32());
    drop(timer);
    println!("");
    output_metadata.describe();

    if args.output_metadata {
        let mut file = fs_err::File::create("output/metadata.json")?;
        serde_json::to_writer(&mut file, &output_metadata)?;
    }

    Ok(())
}

fn write_failed_requests(path: String, counts: &crate::network::Counts) -> Result<()> {
    let mut writer =
        geojson::FeatureWriter::from_writer(std::io::BufWriter::new(fs_err::File::create(path)?));
    for req in &counts.errors_same_endpoints {
        let mut f = req.as_feature();
        f.set_property("reason", "same endpoints");
        writer.write_feature(&f)?;
    }
    for req in &counts.errors_no_path {
        let mut f = req.as_feature();
        f.set_property("reason", "no path");
        writer.write_feature(&f)?;
    }
    Ok(writer.finish()?)
}

#[derive(Serialize, Deserialize)]
pub struct OutputMetadata {
    pub config: crate::config::InputConfig,
    pub num_requests: usize,
    pub num_requests_succeeded: usize,
    pub num_requests_failed: usize,
    pub num_edges_with_count: usize,
    pub num_origins: usize,
    pub num_destinations: usize,
    pub routing_time_seconds: f32,
    pub total_time_seconds: Option<f32>,
    pub tippecanoe_time_seconds: Option<f32>,
    pub total_meters_not_allowed: f64,
    pub total_meters_lts1: f64,
    pub total_meters_lts2: f64,
    pub total_meters_lts3: f64,
    pub total_meters_lts4: f64,
}

impl OutputMetadata {
    pub fn new(
        config: crate::config::InputConfig,
        counts: &crate::network::Counts,
        num_requests: usize,
        routing_time: Duration,
    ) -> Self {
        let totals = counts.total_distance_by_lts;
        let failed = counts.num_errors();
        let succeeded = num_requests.saturating_sub(failed);
        Self {
            config,
            num_requests,
            num_requests_succeeded: succeeded,
            num_requests_failed: failed,
            num_edges_with_count: counts.count_per_edge.len(),
            num_origins: counts.count_per_origin.len(),
            num_destinations: counts.count_per_destination.len(),
            routing_time_seconds: routing_time.as_secs_f32(),
            total_time_seconds: None,
            tippecanoe_time_seconds: None,
            total_meters_not_allowed: totals[0],
            total_meters_lts1: totals[1],
            total_meters_lts2: totals[2],
            total_meters_lts3: totals[3],
            total_meters_lts4: totals[4],
        }
    }

    pub fn describe(&self) {
        println!(
            "Requests: {} total, {} succeeded, {} failed",
            HumanCount(self.num_requests as u64),
            HumanCount(self.num_requests_succeeded as u64),
            HumanCount(self.num_requests_failed as u64),
        );
        println!(
            "Counts generated for {} edges ({} origins, {} destinations)",
            HumanCount(self.num_edges_with_count as u64),
            HumanCount(self.num_origins as u64),
            HumanCount(self.num_destinations as u64),
        );
        let tippecanoe = self
            .tippecanoe_time_seconds
            .map(|seconds| format!("{seconds:.2}s"))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "Routing time: {:.2}s, tippecanoe time: {tippecanoe}",
            self.routing_time_seconds,
        );
    }
}
