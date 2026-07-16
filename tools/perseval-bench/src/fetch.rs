use std::error::Error;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::manifest::SourceManifest;

pub async fn fetch_source(
    manifest_path: &Path,
    output_directory: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let manifest = SourceManifest::load(manifest_path)?;
    fs::create_dir_all(output_directory)?;
    let file_name = Path::new(&manifest.artifact).file_name().ok_or_else(|| {
        format!(
            "manifest artifact has no file name: {:?}",
            manifest.artifact
        )
    })?;
    let output = output_directory.join(file_name);
    if output.is_file() {
        verify_source(&manifest, &output)?;
        println!("verified existing source {}", output.display());
        return Ok(output);
    }

    let temporary = output_directory.join(format!(
        ".{}.partial-{}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let _ = fs::remove_file(&temporary);
    let response = reqwest::Client::new()
        .get(&manifest.url)
        .send()
        .await?
        .error_for_status()?;
    let mut response = response;
    let mut file = File::create(&temporary)?;
    let mut digest = Sha256::new();
    let mut bytes_written = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        digest.update(&chunk);
        file.write_all(&chunk)?;
        bytes_written = bytes_written.saturating_add(chunk.len() as u64);
    }
    file.sync_all()?;
    drop(file);
    let actual = hex::encode(digest.finalize());
    if actual != manifest.sha256 {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "downloaded source hash mismatch: expected {}, received {}",
            manifest.sha256, actual
        )
        .into());
    }
    fs::rename(&temporary, &output)?;
    println!(
        "downloaded and verified {} bytes to {}",
        bytes_written,
        output.display()
    );
    Ok(output)
}

pub fn verify_source(manifest: &SourceManifest, source: &Path) -> Result<(), Box<dyn Error>> {
    let actual = sha256_file(source)?;
    if actual != manifest.sha256 {
        return Err(format!(
            "source hash mismatch for {}: expected {}, received {}",
            source.display(),
            manifest.sha256,
            actual
        )
        .into());
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files_without_loading_them_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture");
        fs::write(&path, b"abc").unwrap();

        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
