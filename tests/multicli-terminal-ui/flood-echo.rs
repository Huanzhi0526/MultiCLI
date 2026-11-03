//! Synthetic helper for the Windows Tauri terminal UI benchmark.
//! It emits paced ASCII output while a second thread acknowledges input lines.

use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let rate_kib = option(&args, "--rate-kib", 100)?.parse::<u64>()?;
    let seconds = option(&args, "--seconds", 60)?.parse::<u64>()?;
    if rate_kib == 0 || seconds == 0 {
        return Err("rate and seconds must be positive".into());
    }

    let stdin = io::stdin();
    let mut start_line = String::new();
    stdin.lock().read_line(&mut start_line)?;
    let target_epoch_ms = start_line
        .trim()
        .strip_prefix("START:")
        .ok_or("expected START:<epoch-ms> barrier")?
        .parse::<u128>()?;
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        if now >= target_epoch_ms {
            break;
        }
        thread::sleep(Duration::from_millis((target_epoch_ms - now).min(25) as u64));
    }

    let output_lock = Arc::new(Mutex::new(()));
    let reader_lock = Arc::clone(&output_lock);
    let reader = thread::Builder::new()
        .name("ui-bench-stdin".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            let mut line = String::new();
            loop {
                line.clear();
                match input.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let marker = line.trim();
                        if let Some(marker) = marker.strip_prefix("PING:") {
                            if let Ok(_guard) = reader_lock.lock() {
                                let mut stdout = io::stdout().lock();
                                let _ = write!(stdout, "\r\nBENCH_ACK:{marker}\r\n");
                                let _ = stdout.flush();
                            }
                        }
                    }
                }
            }
        })?;

    let started = Instant::now();
    let duration = Duration::from_secs(seconds);
    let bytes_per_second = rate_kib * 1024;
    let mut written = 0_u64;
    let chunk = vec![b'x'; 4096];
    while started.elapsed() < duration {
        let due = (started.elapsed().as_secs_f64() * bytes_per_second as f64) as u64;
        while written < due {
            let count = (due - written).min(chunk.len() as u64) as usize;
            let _guard = output_lock.lock().map_err(|_| "output lock poisoned")?;
            let mut stdout = io::stdout().lock();
            stdout.write_all(&chunk[..count])?;
            stdout.flush()?;
            written += count as u64;
        }
        thread::sleep(Duration::from_millis(1));
    }

    let total = bytes_per_second * seconds;
    {
        let _guard = output_lock.lock().map_err(|_| "output lock poisoned")?;
        let mut stdout = io::stdout().lock();
        while written < total {
            let count = (total - written).min(chunk.len() as u64) as usize;
            stdout.write_all(&chunk[..count])?;
            written += count as u64;
        }
        write!(stdout, "\r\nBENCH_DONE:{written}:{}\r\n", started.elapsed().as_millis())?;
        stdout.flush()?;
    }

    // The detached reader is owned by this helper process and ends with it;
    // Windows process teardown releases its console stdin handle.
    drop(reader);
    Ok(())
}

fn option(args: &[String], name: &str, fallback: u64) -> Result<String, String> {
    match args.iter().position(|arg| arg == name) {
        Some(index) => args
            .get(index + 1)
            .cloned()
            .ok_or_else(|| format!("missing value for {name}")),
        None => Ok(fallback.to_string()),
    }
}
