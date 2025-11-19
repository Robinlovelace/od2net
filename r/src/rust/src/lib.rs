use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use bincode::serialize_into;
use fs_err::{self as fs, File};
use od2net::config::{CostFunction, LtsMapping, Uptake};
use od2net::network::{Counts, Edge, Network};
use od2net::requests::Request;
use od2net::router;
use od2net::timer::Timer;
use osm_reader::NodeID;
use savvy::savvy;
use savvy::{OwnedIntegerSexp, OwnedListSexp, OwnedRealSexp, OwnedStringSexp, RealSexp};

/// Run od2net on in-memory OD pairs and return edge-level counts.
#[savvy]
fn run_od2net_counts(
    osm_pbf_path: &str,
    origin_lon: RealSexp,
    origin_lat: RealSexp,
    dest_lon: RealSexp,
    dest_lat: RealSexp,
) -> savvy::Result<savvy::Sexp> {
    let list = route_counts(osm_pbf_path, origin_lon, origin_lat, dest_lon, dest_lat)
        .map_err(|err| savvy::Error::new(err.to_string()))?;
    list.into()
}

fn route_counts(
    osm_pbf_path: &str,
    origin_lon: RealSexp,
    origin_lat: RealSexp,
    dest_lon: RealSexp,
    dest_lat: RealSexp,
) -> Result<OwnedListSexp> {
    let requests = build_requests(&origin_lon, &origin_lat, &dest_lon, &dest_lat)?;

    let paths = IntermediatePaths::new(osm_pbf_path)?;
    let mut timer = Timer::new();
    let mut cost = CostFunction::Distance;
    let lts = LtsMapping::BikeOttawa;
    let mut network = load_or_build_network(&paths.network_bin, osm_pbf_path, &lts, &mut cost, &mut timer)?;

    network.recalculate_cost(&mut cost)?;

    let uptake = Uptake::Identity;
    let ch_path = paths.ch_bin.to_string_lossy().to_string();
    let counts = router::run(&ch_path, &network, requests, &uptake, &mut timer)?;

    build_counts_output(&network, &counts)
}

fn build_requests(
    origin_lon: &RealSexp,
    origin_lat: &RealSexp,
    dest_lon: &RealSexp,
    dest_lat: &RealSexp,
) -> Result<Vec<Request>> {
    let len = origin_lon.len();
    if len == 0 {
        return Err(anyhow!("origin_lon must have at least one element"));
    }
    if origin_lat.len() != len || dest_lon.len() != len || dest_lat.len() != len {
        return Err(anyhow!(
            "origin/destination coordinate vectors must all have equal length"
        ));
    }

    let lon_o = origin_lon.as_slice();
    let lat_o = origin_lat.as_slice();
    let lon_d = dest_lon.as_slice();
    let lat_d = dest_lat.as_slice();

    let mut requests = Vec::with_capacity(len);
    for idx in 0..len {
        let values = [lon_o[idx], lat_o[idx], lon_d[idx], lat_d[idx]];
        if values.iter().any(|v| v.is_nan()) {
            return Err(anyhow!(
                "NA/NaN detected in coordinate vectors at index {}",
                idx + 1
            ));
        }
        requests.push(Request {
            x1: values[0],
            y1: values[1],
            x2: values[2],
            y2: values[3],
            origin: None,
            destination: None,
        });
    }

    Ok(requests)
}

fn load_or_build_network(
    network_bin: &Path,
    osm_pbf_path: &str,
    lts: &LtsMapping,
    cost: &mut CostFunction,
    timer: &mut Timer,
) -> Result<Network> {
    if network_bin.exists() {
        if let Some(path_str) = network_bin.to_str() {
            if let Ok(network) = Network::load_from_bin(path_str) {
                return Ok(network);
            }
        }
    }

    let bytes = fs::read(osm_pbf_path)
        .with_context(|| format!("Failed to read OSM PBF at {osm_pbf_path}"))?;
    let network = Network::make_from_osm(&bytes, lts, cost, timer, None)?;
    let writer = BufWriter::new(File::create(network_bin)?);
    serialize_into(writer, &network)?;
    Ok(network)
}

fn build_counts_output(network: &Network, counts: &Counts) -> Result<OwnedListSexp> {
    let mut from_nodes = Vec::new();
    let mut to_nodes = Vec::new();
    let mut totals = Vec::new();
    let mut lengths = Vec::new();
    let mut lts_values = Vec::new();
    let mut wkt = Vec::new();

    for ((node1, node2), count) in &counts.count_per_edge {
        if *count == 0.0 {
            continue;
        }
        if let Some(edge) = network.edges.get(&(*node1, *node2)) {
            add_row(
                edge,
                *count,
                *node1,
                *node2,
                &mut from_nodes,
                &mut to_nodes,
                &mut totals,
                &mut lengths,
                &mut lts_values,
                &mut wkt,
            );
            continue;
        }
        if let Some(edge) = network.edges.get(&(*node2, *node1)) {
            add_row(
                edge,
                *count,
                *node1,
                *node2,
                &mut from_nodes,
                &mut to_nodes,
                &mut totals,
                &mut lengths,
                &mut lts_values,
                &mut wkt,
            );
        }
    }

    let mut out = convert_savvy(OwnedListSexp::new(6, true), "allocate counts output")?;
    convert_savvy(
        out.set_name_and_value(
            0,
            "from_node",
            convert_savvy(OwnedRealSexp::try_from_slice(&from_nodes), "from_node slice")?,
        ),
        "assign from_node",
    )?;
    convert_savvy(
        out.set_name_and_value(
            1,
            "to_node",
            convert_savvy(OwnedRealSexp::try_from_slice(&to_nodes), "to_node slice")?,
        ),
        "assign to_node",
    )?;
    convert_savvy(
        out.set_name_and_value(
            2,
            "count",
            convert_savvy(OwnedRealSexp::try_from_slice(&totals), "count slice")?,
        ),
        "assign count",
    )?;
    convert_savvy(
        out.set_name_and_value(
            3,
            "length_m",
            convert_savvy(OwnedRealSexp::try_from_slice(&lengths), "length slice")?,
        ),
        "assign length_m",
    )?;
    convert_savvy(
        out.set_name_and_value(
            4,
            "lts",
            convert_savvy(OwnedIntegerSexp::try_from_slice(&lts_values), "lts slice")?,
        ),
        "assign lts",
    )?;
    convert_savvy(
        out.set_name_and_value(
            5,
            "wkt",
            convert_savvy(OwnedStringSexp::try_from_slice(&wkt), "wkt slice")?,
        ),
        "assign wkt",
    )?;
    Ok(out)
}

fn add_row(
    edge: &Edge,
    count: f64,
    node1: NodeID,
    node2: NodeID,
    from_nodes: &mut Vec<f64>,
    to_nodes: &mut Vec<f64>,
    totals: &mut Vec<f64>,
    lengths: &mut Vec<f64>,
    lts_values: &mut Vec<i32>,
    wkt: &mut Vec<String>,
) {
    from_nodes.push(node1.0 as f64);
    to_nodes.push(node2.0 as f64);
    totals.push(count);
    lengths.push(edge.length_meters);
    lts_values.push(edge.lts as u8 as i32);
    wkt.push(line_string(edge, node1, node2));
}

fn line_string(edge: &Edge, node1: NodeID, node2: NodeID) -> String {
    use geojson::Value::LineString;

    let feature = edge.to_geojson_for_detailed_output(node1, node2, true);
    if let Some(geometry) = feature.geometry {
        if let LineString(coords) = geometry.value {
            if coords.is_empty() {
                return "LINESTRING EMPTY".to_string();
            }
            let parts: Vec<String> = coords
                .into_iter()
                .map(|pt| format!("{} {}", pt[0], pt[1]))
                .collect();
            return format!("LINESTRING({})", parts.join(", "));
        }
    }
    "LINESTRING EMPTY".to_string()
}

fn convert_savvy<T>(res: savvy::Result<T>, context: &str) -> Result<T> {
    res.map_err(|err| anyhow!("{context}: {err}"))
}

struct IntermediatePaths {
    network_bin: PathBuf,
    ch_bin: PathBuf,
}

impl IntermediatePaths {
    fn new(osm_pbf_path: &str) -> Result<Self> {
        let pbf_path = Path::new(osm_pbf_path);
        let parent = pbf_path
            .parent()
            .ok_or_else(|| anyhow!("{osm_pbf_path} has no parent directory"))?;
        let intermediate = parent.join("intermediate");
        fs::create_dir_all(&intermediate)?;
        Ok(Self {
            network_bin: intermediate.join("network.bin"),
            ch_bin: intermediate.join("ch.bin"),
        })
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use savvy::OwnedRealSexp;
//
//     #[test]
//     fn rejects_mismatched_lengths() {
//         let data = vec![0.0, 1.0];
//         let x = OwnedRealSexp::try_from_slice(&data).unwrap().as_read_only();
//         let short = OwnedRealSexp::try_from_slice(&data[..1]).unwrap().as_read_only();
//         let err = build_requests(&x, &x, &x, &short).unwrap_err();
//         assert!(err.to_string().contains("equal length"));
//     }
//
//     #[test]
//     fn rejects_nans() {
//         let nan = f64::NAN;
//         let vals = vec![0.0, nan];
//         let ok = OwnedRealSexp::try_from_slice(&vals[..1]).unwrap().as_read_only();
//         let bad = OwnedRealSexp::try_from_slice(&vals).unwrap().as_read_only();
//         let err = build_requests(&ok, &ok, &ok, &bad).unwrap_err();
//         assert!(err.to_string().contains("NA/NaN"));
//     }
// }
