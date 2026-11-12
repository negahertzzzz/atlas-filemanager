#![allow(clippy::disallowed_methods)]
// reason: model previews run on blocking workers and must read files.

//! Dependency-free 3D previews: GLB/glTF, binary FBX, OBJ and STL are parsed
//! into a triangle soup and drawn by a tiny flat-shaded software rasterizer
//! (2x supersampled, transparent background). Used for inspector previews and
//! grid thumbnails; no GPU, no extra crates beyond `serde_json`/`flate2`.

use std::io::Read as _;
use std::path::Path;

use atlas_model::preview_cache::PreviewResult;

/// Models larger than this are not parsed for previews.
const MAX_MODEL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Default)]
struct Mesh {
    positions: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
}

impl Mesh {
    fn push_polygon(&mut self, base: u32, polygon: &[u32]) {
        for i in 1..polygon.len().saturating_sub(1) {
            self.triangles
                .push([base + polygon[0], base + polygon[i], base + polygon[i + 1]]);
        }
    }
}

pub(crate) fn is_model_ext(ext: &str) -> bool {
    atlas_model::preview_cache::is_model_ext(ext)
}

/// Inspector preview: a rendered still plus mesh statistics.
pub(crate) fn preview_model(path: &Path) -> PreviewResult {
    let ext = extension(path);
    match load_mesh(path, &ext) {
        Ok(mesh) => {
            let (rgba, width, height) = render(&mesh, 1024);
            PreviewResult::Image {
                rgba,
                width,
                height,
                original_width: width,
                original_height: height,
                exif: vec![
                    ("Format".into(), ext.to_uppercase()),
                    ("Triangles".into(), mesh.triangles.len().to_string()),
                    ("Vertices".into(), mesh.positions.len().to_string()),
                ],
            }
        }
        Err(error) => PreviewResult::Error(error),
    }
}

/// Grid/multi-selection thumbnail.
pub fn model_thumbnail(path: &Path, max_edge: u32) -> Option<(Vec<u8>, usize, usize)> {
    let mesh = load_mesh(path, &extension(path)).ok()?;
    Some(render(&mesh, max_edge.clamp(64, 1024) as usize))
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn load_mesh(path: &Path, ext: &str) -> Result<Mesh, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_MODEL_BYTES {
        return Err("Model is too large to preview".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mesh = match ext {
        "glb" => parse_glb(&bytes)?,
        "gltf" => {
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            parse_gltf(&json, None, path.parent())?
        }
        "fbx" => parse_fbx(&bytes)?,
        "obj" => parse_obj(&String::from_utf8_lossy(&bytes)),
        "stl" => parse_stl(&bytes),
        _ => return Err(format!("Unsupported model format .{ext}")),
    };
    if mesh.triangles.is_empty() {
        return Err("Model has no triangle geometry".into());
    }
    Ok(mesh)
}

// ── glTF / GLB ───────────────────────────────────────────────────────────────

fn parse_glb(bytes: &[u8]) -> Result<Mesh, String> {
    if bytes.len() < 20 || &bytes[..4] != b"glTF" {
        return Err("Not a GLB file".into());
    }
    let mut offset = 12;
    let (mut json, mut bin) = (None, None);
    while offset + 8 <= bytes.len() {
        let len = u32_at(bytes, offset) as usize;
        let kind = u32_at(bytes, offset + 4);
        let data = bytes
            .get(offset + 8..offset + 8 + len)
            .ok_or("Truncated GLB chunk")?;
        match kind {
            0x4E4F_534A => json = Some(data),
            0x004E_4942 => bin = Some(data),
            _ => {}
        }
        offset += 8 + len;
    }
    let json: serde_json::Value =
        serde_json::from_slice(json.ok_or("GLB has no JSON chunk")?).map_err(|e| e.to_string())?;
    parse_gltf(&json, bin, None)
}

fn parse_gltf(
    json: &serde_json::Value,
    glb_bin: Option<&[u8]>,
    dir: Option<&Path>,
) -> Result<Mesh, String> {
    if json["extensionsRequired"]
        .as_array()
        .is_some_and(|ext| ext.iter().any(|e| e == "KHR_draco_mesh_compression"))
    {
        return Err("Draco-compressed glTF is not supported".into());
    }
    let buffers: Vec<Vec<u8>> = json["buffers"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|buffer| match buffer["uri"].as_str() {
                    None => glb_bin.map(<[u8]>::to_vec).unwrap_or_default(),
                    Some(uri) => load_uri(uri, dir).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut mesh = Mesh::default();
    let add_mesh = |index: usize, matrix: &[f32; 16], mesh: &mut Mesh| {
        for primitive in json["meshes"][index]["primitives"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let mode = primitive["mode"].as_u64().unwrap_or(4);
            let Some(position) = primitive["attributes"]["POSITION"].as_u64() else {
                continue;
            };
            let Some(points) = read_vec3(json, &buffers, position as usize) else {
                continue;
            };
            let indices = match primitive["indices"].as_u64() {
                Some(accessor) => read_indices(json, &buffers, accessor as usize),
                None => Some((0..points.len() as u32).collect()),
            };
            let Some(indices) = indices else { continue };
            let base = mesh.positions.len() as u32;
            mesh.positions
                .extend(points.iter().map(|p| transform(matrix, *p)));
            match mode {
                4 => {
                    for tri in indices.chunks_exact(3) {
                        mesh.triangles
                            .push([base + tri[0], base + tri[1], base + tri[2]]);
                    }
                }
                5 => {
                    for i in 2..indices.len() {
                        let (a, b) = if i % 2 == 0 {
                            (i - 2, i - 1)
                        } else {
                            (i - 1, i - 2)
                        };
                        mesh.triangles.push([
                            base + indices[a],
                            base + indices[b],
                            base + indices[i],
                        ]);
                    }
                }
                6 => mesh.push_polygon(base, &indices),
                _ => {}
            }
        }
    };
    let roots: Vec<usize> = json["scenes"][json["scene"].as_u64().unwrap_or(0) as usize]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.as_u64().map(|n| n as usize))
                .collect()
        })
        .unwrap_or_default();
    if roots.is_empty() {
        let count = json["meshes"].as_array().map_or(0, Vec::len);
        for index in 0..count {
            add_mesh(index, &IDENTITY, &mut mesh);
        }
    } else {
        let mut stack: Vec<(usize, [f32; 16], u32)> =
            roots.into_iter().map(|n| (n, IDENTITY, 0)).collect();
        while let Some((node_index, parent, depth)) = stack.pop() {
            let node = &json["nodes"][node_index];
            if depth > 64 || node.is_null() {
                continue;
            }
            let world = multiply(&parent, &node_matrix(node));
            if let Some(index) = node["mesh"].as_u64() {
                add_mesh(index as usize, &world, &mut mesh);
            }
            for child in node["children"].as_array().into_iter().flatten() {
                if let Some(child) = child.as_u64() {
                    stack.push((child as usize, world, depth + 1));
                }
            }
        }
    }
    Ok(mesh)
}

fn load_uri(uri: &str, dir: Option<&Path>) -> Option<Vec<u8>> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (_, data) = rest.split_once(";base64,")?;
        return decode_base64(data);
    }
    if uri.contains("..") || uri.contains(':') {
        return None;
    }
    std::fs::read(dir?.join(uri)).ok()
}

fn accessor_view<'a>(
    json: &serde_json::Value,
    buffers: &'a [Vec<u8>],
    accessor: usize,
) -> Option<(&'a [u8], usize, usize, u64)> {
    let acc = &json["accessors"][accessor];
    let view = &json["bufferViews"][acc["bufferView"].as_u64()? as usize];
    let buffer = buffers.get(view["buffer"].as_u64()? as usize)?;
    let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = view["byteStride"].as_u64().unwrap_or(0) as usize;
    let count = acc["count"].as_u64()? as usize;
    let component = acc["componentType"].as_u64()?;
    Some((buffer.get(start..)?, stride, count, component))
}

fn read_vec3(
    json: &serde_json::Value,
    buffers: &[Vec<u8>],
    accessor: usize,
) -> Option<Vec<[f32; 3]>> {
    let (data, stride, count, component) = accessor_view(json, buffers, accessor)?;
    if component != 5126 {
        return None;
    }
    let stride = if stride == 0 { 12 } else { stride };
    (0..count)
        .map(|i| {
            let at = i * stride;
            let chunk = data.get(at..at + 12)?;
            Some([f32_at(chunk, 0), f32_at(chunk, 4), f32_at(chunk, 8)])
        })
        .collect()
}

fn read_indices(
    json: &serde_json::Value,
    buffers: &[Vec<u8>],
    accessor: usize,
) -> Option<Vec<u32>> {
    let (data, stride, count, component) = accessor_view(json, buffers, accessor)?;
    let width = match component {
        5121 => 1,
        5123 => 2,
        5125 => 4,
        _ => return None,
    };
    let stride = if stride == 0 { width } else { stride };
    (0..count)
        .map(|i| {
            let at = i * stride;
            let chunk = data.get(at..at + width)?;
            Some(match width {
                1 => u32::from(chunk[0]),
                2 => u32::from(u16::from_le_bytes([chunk[0], chunk[1]])),
                _ => u32_at(chunk, 0),
            })
        })
        .collect()
}

const IDENTITY: [f32; 16] = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];

/// Column-major node matrix from `matrix` or TRS.
fn node_matrix(node: &serde_json::Value) -> [f32; 16] {
    let floats = |key: &str, n: usize| -> Option<Vec<f32>> {
        let list = node[key].as_array()?;
        (list.len() == n).then(|| {
            list.iter()
                .map(|v| v.as_f64().unwrap_or(0.) as f32)
                .collect()
        })
    };
    if let Some(m) = floats("matrix", 16) {
        let mut out = [0.; 16];
        out.copy_from_slice(&m);
        return out;
    }
    let t = floats("translation", 3).unwrap_or(vec![0., 0., 0.]);
    let r = floats("rotation", 4).unwrap_or(vec![0., 0., 0., 1.]);
    let s = floats("scale", 3).unwrap_or(vec![1., 1., 1.]);
    let (x, y, z, w) = (r[0], r[1], r[2], r[3]);
    [
        (1. - 2. * (y * y + z * z)) * s[0],
        (2. * (x * y + z * w)) * s[0],
        (2. * (x * z - y * w)) * s[0],
        0.,
        (2. * (x * y - z * w)) * s[1],
        (1. - 2. * (x * x + z * z)) * s[1],
        (2. * (y * z + x * w)) * s[1],
        0.,
        (2. * (x * z + y * w)) * s[2],
        (2. * (y * z - x * w)) * s[2],
        (1. - 2. * (x * x + y * y)) * s[2],
        0.,
        t[0],
        t[1],
        t[2],
        1.,
    ]
}

fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}

fn transform(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ── FBX (binary) ─────────────────────────────────────────────────────────────

fn parse_fbx(bytes: &[u8]) -> Result<Mesh, String> {
    if !bytes.starts_with(b"Kaydara FBX Binary") {
        return Err("Only binary FBX files can be previewed".into());
    }
    let version = u32_at(bytes, 23);
    let mut mesh = Mesh::default();
    let mut reader = FbxReader {
        bytes,
        wide: version >= 7500,
    };
    reader.walk(27, bytes.len(), 0, &mut mesh)?;
    Ok(mesh)
}

struct FbxReader<'a> {
    bytes: &'a [u8],
    wide: bool,
}

enum FbxArray {
    F64(Vec<f64>),
    I32(Vec<i32>),
    Other,
}

impl FbxReader<'_> {
    fn word(&self, at: usize) -> Result<(u64, usize), String> {
        if self.wide {
            let b = self.bytes.get(at..at + 8).ok_or("Truncated FBX")?;
            Ok((u64::from_le_bytes(b.try_into().unwrap_or_default()), at + 8))
        } else {
            let b = self.bytes.get(at..at + 4).ok_or("Truncated FBX")?;
            Ok((u64::from(u32_at(b, 0)), at + 4))
        }
    }

    /// Walk sibling node records in `[at, end)`, collecting Geometry meshes.
    fn walk(
        &mut self,
        mut at: usize,
        end: usize,
        depth: u32,
        mesh: &mut Mesh,
    ) -> Result<(), String> {
        if depth > 32 {
            return Ok(());
        }
        while at < end {
            let (node_end, next) = self.word(at)?;
            let (_props, next) = self.word(next)?;
            let (props_len, next) = self.word(next)?;
            let name_len = *self.bytes.get(next).ok_or("Truncated FBX")? as usize;
            if node_end == 0 {
                return Ok(());
            }
            let name = self
                .bytes
                .get(next + 1..next + 1 + name_len)
                .ok_or("Truncated FBX")?;
            let props_at = next + 1 + name_len;
            let children_at = props_at + props_len as usize;
            let node_end = node_end as usize;
            if node_end <= at || node_end > self.bytes.len() {
                return Err("Corrupt FBX node".into());
            }
            if name == b"Geometry" {
                self.read_geometry(children_at, node_end, mesh)?;
            } else if name == b"Objects" {
                self.walk(children_at, node_end, depth + 1, mesh)?;
            }
            at = node_end;
        }
        Ok(())
    }

    fn read_geometry(&self, mut at: usize, end: usize, mesh: &mut Mesh) -> Result<(), String> {
        let (mut vertices, mut polygons) = (None, None);
        while at < end {
            let (node_end, next) = self.word(at)?;
            let (_, next) = self.word(next)?;
            let (_, next) = self.word(next)?;
            if node_end == 0 {
                break;
            }
            let name_len = *self.bytes.get(next).ok_or("Truncated FBX")? as usize;
            let name = self
                .bytes
                .get(next + 1..next + 1 + name_len)
                .ok_or("Truncated FBX")?;
            let props_at = next + 1 + name_len;
            match name {
                b"Vertices" => vertices = Some(self.read_array(props_at)?),
                b"PolygonVertexIndex" => polygons = Some(self.read_array(props_at)?),
                _ => {}
            }
            if node_end as usize <= at {
                break;
            }
            at = node_end as usize;
        }
        let (Some(FbxArray::F64(vertices)), Some(FbxArray::I32(polygons))) = (vertices, polygons)
        else {
            return Ok(());
        };
        let base = mesh.positions.len() as u32;
        let count = (vertices.len() / 3) as u32;
        mesh.positions.extend(
            vertices
                .chunks_exact(3)
                .map(|v| [v[0] as f32, v[1] as f32, v[2] as f32]),
        );
        let mut polygon = Vec::with_capacity(8);
        for &index in &polygons {
            let (index, last) = if index < 0 {
                (!index, true)
            } else {
                (index, false)
            };
            if (index as u32) < count {
                polygon.push(index as u32);
            }
            if last {
                mesh.push_polygon(base, &polygon);
                polygon.clear();
            }
        }
        Ok(())
    }

    fn read_array(&self, at: usize) -> Result<FbxArray, String> {
        let kind = *self.bytes.get(at).ok_or("Truncated FBX")?;
        if !matches!(kind, b'd' | b'f' | b'i' | b'l') {
            return Ok(FbxArray::Other);
        }
        let header = self.bytes.get(at + 1..at + 13).ok_or("Truncated FBX")?;
        let (len, encoding, compressed) = (
            u32_at(header, 0) as usize,
            u32_at(header, 4),
            u32_at(header, 8) as usize,
        );
        let raw = self
            .bytes
            .get(at + 13..at + 13 + compressed)
            .ok_or("Truncated FBX array")?;
        let width = if matches!(kind, b'd' | b'l') { 8 } else { 4 };
        let data = if encoding == 1 {
            let mut out = Vec::with_capacity(len * width);
            flate2::read::ZlibDecoder::new(raw)
                .take((len * width) as u64)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            out
        } else {
            raw.to_vec()
        };
        Ok(match kind {
            b'd' => FbxArray::F64(
                data.chunks_exact(8)
                    .map(|c| f64::from_le_bytes(c.try_into().unwrap_or_default()))
                    .collect(),
            ),
            b'f' => FbxArray::F64(
                data.chunks_exact(4)
                    .map(|c| f64::from(f32_at(c, 0)))
                    .collect(),
            ),
            b'i' => FbxArray::I32(
                data.chunks_exact(4)
                    .map(|c| i32::from_le_bytes(c.try_into().unwrap_or_default()))
                    .collect(),
            ),
            _ => FbxArray::Other,
        })
    }
}

// ── OBJ / STL ────────────────────────────────────────────────────────────────

fn parse_obj(text: &str) -> Mesh {
    let mut mesh = Mesh::default();
    let mut polygon = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("v") => {
                let v: Vec<f32> = parts.take(3).filter_map(|p| p.parse().ok()).collect();
                if v.len() == 3 {
                    mesh.positions.push([v[0], v[1], v[2]]);
                }
            }
            Some("f") => {
                let count = mesh.positions.len() as i64;
                polygon.clear();
                for part in parts {
                    let Some(Ok(index)) = part.split('/').next().map(str::parse::<i64>) else {
                        continue;
                    };
                    let index = if index < 0 { count + index } else { index - 1 };
                    if (0..count).contains(&index) {
                        polygon.push(index as u32);
                    }
                }
                mesh.push_polygon(0, &polygon);
            }
            _ => {}
        }
    }
    mesh
}

fn parse_stl(bytes: &[u8]) -> Mesh {
    let mut mesh = Mesh::default();
    let binary_count = (bytes.len() >= 84).then(|| u32_at(bytes, 80) as usize);
    if binary_count.is_some_and(|n| 84 + n * 50 == bytes.len()) {
        for tri in bytes[84..].chunks_exact(50) {
            let base = mesh.positions.len() as u32;
            for v in 0..3 {
                let at = 12 + v * 12;
                mesh.positions
                    .push([f32_at(tri, at), f32_at(tri, at + 4), f32_at(tri, at + 8)]);
            }
            mesh.triangles.push([base, base + 1, base + 2]);
        }
    } else {
        for line in String::from_utf8_lossy(bytes).lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("vertex") {
                let v: Vec<f32> = parts.take(3).filter_map(|p| p.parse().ok()).collect();
                if v.len() == 3 {
                    mesh.positions.push([v[0], v[1], v[2]]);
                    if mesh.positions.len() % 3 == 0 {
                        let base = mesh.positions.len() as u32 - 3;
                        mesh.triangles.push([base, base + 1, base + 2]);
                    }
                }
            }
        }
    }
    // STL is conventionally Z-up; the renderer is Y-up.
    for p in &mut mesh.positions {
        *p = [p[0], p[2], -p[1]];
    }
    mesh
}

// ── Rasterizer ───────────────────────────────────────────────────────────────

/// Flat-shaded three-quarter view, 2x supersampled, transparent background.
fn render(mesh: &Mesh, edge: usize) -> (Vec<u8>, usize, usize) {
    const SS: usize = 2;
    let size = edge * SS;
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in &mesh.positions {
        for axis in 0..3 {
            if p[axis].is_finite() {
                min[axis] = min[axis].min(p[axis]);
                max[axis] = max[axis].max(p[axis]);
            }
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let (yaw, pitch) = (-35f32.to_radians(), 22f32.to_radians());
    let (sy, cy, sp, cp) = (yaw.sin(), yaw.cos(), pitch.sin(), pitch.cos());
    let rotate = |p: [f32; 3]| {
        let (x, y, z) = (p[0] - center[0], p[1] - center[1], p[2] - center[2]);
        let (x, z) = (x * cy + z * sy, -x * sy + z * cy);
        let (y, z) = (y * cp - z * sp, y * sp + z * cp);
        [x, y, z]
    };
    let rotated: Vec<[f32; 3]> = mesh.positions.iter().map(|p| rotate(*p)).collect();
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in &rotated {
        if p[0].is_finite() && p[1].is_finite() {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
    }
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1e-6);
    let scale = size as f32 * 0.86 / extent;
    let (mid_x, mid_y) = ((lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5);
    let screen: Vec<[f32; 3]> = rotated
        .iter()
        .map(|p| {
            [
                (p[0] - mid_x) * scale + size as f32 * 0.5,
                -(p[1] - mid_y) * scale + size as f32 * 0.5,
                p[2],
            ]
        })
        .collect();
    let light = normalize([-0.35, 0.65, 0.68]);
    let mut depth = vec![f32::MIN; size * size];
    let mut shade = vec![0u8; size * size];
    for tri in &mesh.triangles {
        let (Some(a), Some(b), Some(c)) = (
            screen.get(tri[0] as usize),
            screen.get(tri[1] as usize),
            screen.get(tri[2] as usize),
        ) else {
            continue;
        };
        let (ra, rb, rc) = (
            rotated[tri[0] as usize],
            rotated[tri[1] as usize],
            rotated[tri[2] as usize],
        );
        let normal = normalize(cross(sub(rb, ra), sub(rc, ra)));
        let lit = 0.28 + 0.72 * dot(normal, light).abs();
        let value = (lit.clamp(0., 1.) * 254.) as u8 + 1;
        let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        if area.abs() < 1e-9 || !area.is_finite() {
            continue;
        }
        let x0 = a[0].min(b[0]).min(c[0]).floor().max(0.) as usize;
        let y0 = a[1].min(b[1]).min(c[1]).floor().max(0.) as usize;
        let x1 = (a[0].max(b[0]).max(c[0]).ceil() as usize).min(size - 1);
        let y1 = (a[1].max(b[1]).max(c[1]).ceil() as usize).min(size - 1);
        for y in y0..=y1 {
            let py = y as f32 + 0.5;
            for x in x0..=x1 {
                let px = x as f32 + 0.5;
                let w0 = ((b[0] - px) * (c[1] - py) - (b[1] - py) * (c[0] - px)) / area;
                let w1 = ((c[0] - px) * (a[1] - py) - (c[1] - py) * (a[0] - px)) / area;
                let w2 = 1. - w0 - w1;
                if w0 < 0. || w1 < 0. || w2 < 0. {
                    continue;
                }
                let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                let at = y * size + x;
                if z > depth[at] {
                    depth[at] = z;
                    shade[at] = value;
                }
            }
        }
    }
    // Downsample with coverage alpha; tint a cool neutral clay.
    let mut rgba = vec![0u8; edge * edge * 4];
    for y in 0..edge {
        for x in 0..edge {
            let (mut sum, mut covered) = (0u32, 0u32);
            for dy in 0..SS {
                for dx in 0..SS {
                    let value = shade[(y * SS + dy) * size + x * SS + dx];
                    if value > 0 {
                        sum += u32::from(value);
                        covered += 1;
                    }
                }
            }
            if covered == 0 {
                continue;
            }
            let lit = sum as f32 / covered as f32 / 255.;
            let at = (y * edge + x) * 4;
            rgba[at] = (lit * 206.) as u8;
            rgba[at + 1] = (lit * 212.) as u8;
            rgba[at + 2] = (lit * 224.) as u8;
            rgba[at + 3] = (covered * 255 / (SS * SS) as u32) as u8;
        }
    }
    (rgba, edge, edge)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt().max(1e-12);
    [v[0] / len, v[1] / len, v[2] / len]
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_bits(u32_at(bytes, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_obj() -> String {
        let mut text = String::new();
        for z in [0., 1.] {
            for y in [0., 1.] {
                for x in [0., 1.] {
                    text.push_str(&format!("v {x} {y} {z}\n"));
                }
            }
        }
        for face in [
            "1 2 4 3", "5 7 8 6", "1 5 6 2", "3 4 8 7", "1 3 7 5", "2 6 8 4",
        ] {
            text.push_str(&format!("f {face}\n"));
        }
        text
    }

    #[test]
    fn obj_cube_renders_an_opaque_centered_silhouette() {
        let mesh = parse_obj(&cube_obj());
        assert_eq!(mesh.triangles.len(), 12);
        let (rgba, w, h) = render(&mesh, 64);
        assert_eq!((w, h), (64, 64));
        assert!(
            rgba[(32 * 64 + 32) * 4 + 3] == 255,
            "center must be covered"
        );
        assert_eq!(rgba[3], 0, "corner stays transparent");
    }

    #[test]
    fn glb_with_trs_node_is_parsed() {
        let mut bin = Vec::new();
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 1., 0.]] {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let json = serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0, "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "translation": [5.0, 0.0, 0.0]}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "bufferViews": [{"buffer": 0, "byteLength": 36}],
            "buffers": [{"byteLength": 36}]
        });
        let mut json = serde_json::to_vec(&json).unwrap();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let mut glb = b"glTF".to_vec();
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
        glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        let mesh = parse_glb(&glb).unwrap();
        assert_eq!(mesh.triangles.len(), 1);
        assert_eq!(mesh.positions[1], [6., 0., 0.]);
    }

    #[test]
    fn binary_stl_is_detected_by_size() {
        let mut stl = vec![0u8; 80];
        stl.extend_from_slice(&1u32.to_le_bytes());
        let mut tri = vec![0u8; 12];
        for v in [[0f32, 0., 0.], [1., 0., 0.], [0., 0., 1.]] {
            for c in v {
                tri.extend_from_slice(&c.to_le_bytes());
            }
        }
        tri.extend_from_slice(&[0, 0]);
        stl.extend_from_slice(&tri);
        let mesh = parse_stl(&stl);
        assert_eq!(mesh.triangles.len(), 1);
        assert_eq!(mesh.positions[2], [0., 1., 0.]);
    }

    /// `ATLAS_MODEL_PROBE="a.glb;b.fbx" cargo test -p atlas-core model_probe -- --ignored`
    /// renders real files to `target/model-probe/*.png`.
    #[test]
    #[ignore = "manual probe over local model files"]
    fn model_probe() {
        let list = std::env::var("ATLAS_MODEL_PROBE").unwrap_or_default();
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/model-probe");
        std::fs::create_dir_all(&out).unwrap();
        for path in list.split(';').filter(|p| !p.is_empty()) {
            let path = std::path::Path::new(path);
            let started = std::time::Instant::now();
            match load_mesh(path, &extension(path)) {
                Ok(mesh) => {
                    let (rgba, w, h) = render(&mesh, 512);
                    let name = path.file_name().unwrap().to_string_lossy();
                    image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                        .unwrap()
                        .save(out.join(format!("{name}.png")))
                        .unwrap();
                    eprintln!(
                        "{name}: {} tris in {:?}",
                        mesh.triangles.len(),
                        started.elapsed()
                    );
                }
                Err(error) => eprintln!("{}: {error}", path.display()),
            }
        }
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
    }
}
