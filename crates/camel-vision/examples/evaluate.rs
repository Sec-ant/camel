//! Evaluate local models without copying source images or writing page-sized previews.
use std::{
    error::Error,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Instant,
};

use camel_vision::Engine;
use serde_json::{Value, json};

fn fingerprint(path: &Path) -> Result<Value, Box<dyn Error>> {
    let metadata = fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(json!({"size": metadata.len(), "modified_ns": modified}))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 3 || args.len() > 4 {
        return Err("usage: evaluate MODELS INPUT OUTPUT [JOBS]".into());
    }
    let models = PathBuf::from(&args[0]);
    let input = PathBuf::from(&args[1]).canonicalize()?;
    let output = PathBuf::from(&args[2]);
    let jobs: usize = args
        .get(3)
        .map(|n| n.to_string_lossy().parse())
        .transpose()?
        .unwrap_or(2);
    if jobs == 0 {
        return Err("JOBS must be positive".into());
    }
    let mut paths: Vec<_> = fs::read_dir(&input)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|path| {
        path.is_file()
            && path.extension().is_some_and(|ext| {
                ["jpg", "jpeg", "png"]
                    .iter()
                    .any(|suffix| ext.eq_ignore_ascii_case(suffix))
            })
    });
    paths.sort();
    if paths.is_empty() {
        return Err("no images".into());
    }
    fs::create_dir_all(&output)?;
    let manifest_path = output.join("run.json");
    let manifest = json!({
        "input": input,
        "detector": fingerprint(&models.join("detector.onnx"))?,
        "recognizer": fingerprint(&models.join("recognizer.onnx"))?,
        "executable": fingerprint(&std::env::current_exe()?)?,
        "pages": paths.len(),
    });
    if manifest_path.exists() {
        let previous: Value = serde_json::from_reader(BufReader::new(File::open(&manifest_path)?))?;
        if previous != manifest {
            return Err(
                "existing evaluation belongs to different inputs, models, or executable".into(),
            );
        }
    } else {
        if fs::read_dir(&output)?.next().is_some() {
            return Err("existing evaluation directory has no run manifest".into());
        }
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&manifest_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &manifest)?;
        writer.flush()?;
    }
    let engine = Engine::load(&models, jobs)?;
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let resumed = AtomicUsize::new(0);
    let start = Instant::now();
    thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs.min(paths.len()))
            .map(|_| {
                scope.spawn(|| -> Result<(), String> {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = paths.get(i) else {
                            break;
                        };
                        let file_path = output.join(format!("{:05}.json", i + 1));
                        let source_fingerprint = fingerprint(path).map_err(|e| e.to_string())?;
                        if file_path.exists()
                            && let Ok(file) = File::open(&file_path)
                            && let Ok(record) =
                                serde_json::from_reader::<_, Value>(BufReader::new(file))
                            && record.get("error").is_none()
                            && record.get("source") == Some(&json!(path))
                            && record.get("source_fingerprint") == Some(&source_fingerprint)
                        {
                            resumed.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        let started = Instant::now();
                        let result = (|| -> Result<_, Box<dyn Error>> {
                            let mut image = image::open(path)?.to_rgb8();
                            let (page, metadata) = engine.analyze(&image)?;
                            for plan in camel::geometry::prepare_erasures(
                                &image,
                                page.regions.iter().map(|r| &r.bubble),
                            )? {
                                plan.apply(&mut image)?;
                            }
                            Ok(json!({"page":page,"analysis":metadata}))
                        })();
                        let mut record = match result {
                            Ok(value) => value,
                            Err(error) => {
                                failed.fetch_add(1, Ordering::Relaxed);
                                json!({"error":error.to_string()})
                            }
                        };
                        record["source"] = json!(path);
                        record["source_fingerprint"] = source_fingerprint;
                        record["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
                        let temporary =
                            output.join(format!(".{:05}.{}.tmp", i + 1, std::process::id()));
                        let file = File::create(&temporary).map_err(|e| e.to_string())?;
                        let mut writer = BufWriter::new(file);
                        serde_json::to_writer(&mut writer, &record).map_err(|e| e.to_string())?;
                        writer.flush().map_err(|e| e.to_string())?;
                        fs::rename(&temporary, &file_path).map_err(|e| e.to_string())?;
                        eprintln!(
                            "[{}/{}] {:.2}s {}",
                            i + 1,
                            paths.len(),
                            started.elapsed().as_secs_f64(),
                            path.display()
                        );
                    }
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle.join().map_err(|_| "worker panicked")??;
        }
        Ok::<_, String>(())
    })?;
    let failed = failed.load(Ordering::Relaxed);
    let mut writer = BufWriter::new(File::create(output.join("summary.json"))?);
    serde_json::to_writer_pretty(
        &mut writer,
        &json!({"pages":paths.len(),"failed":failed,"resumed":resumed.load(Ordering::Relaxed),"elapsed_seconds":start.elapsed().as_secs_f64(),"jobs":jobs}),
    )?;
    writer.flush()?;
    if failed != 0 {
        return Err(format!("{failed} pages failed; see per-page records").into());
    }
    Ok(())
}
