// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Signs every Mach-O inside the macOS unit's carried wheels and carried Python archive with a
//! Developer ID, so a bundle that carries them passes Apple's notary, which opens both kinds of
//! archive and refuses any Mach-O in them not signed with a Developer ID and a secure timestamp.
//!
//! Signing changes each archive's bytes, so a wheel's `RECORD` is rewritten to the signed
//! members' sha256 and size, and the Python archive's new sha256 is what the unit's uv download
//! metadata names (see [`write_the_uv_python_download_metadata`]).

use anyhow::{Context, Result, bail};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Signs one Mach-O file in place.
pub trait MachOFileSigner {
    /// Sign `mach_o_file` in place, with `entitlements` when it is an executable that needs them.
    fn sign_the_mach_o_file(&self, mach_o_file: &Path, entitlements: Option<&Path>) -> Result<()>;
}

/// Signs with `codesign`, the hardened runtime and a secure timestamp, under one
/// `Developer ID Application: …` identity in a keychain on the search list.
pub struct CodesignWithADeveloperIdApplicationIdentity {
    developer_id_application_identity: String,
}

impl CodesignWithADeveloperIdApplicationIdentity {
    /// Refuses an identity that is not a `Developer ID Application: …` one: the notary accepts
    /// no other.
    pub fn for_the_identity(developer_id_application_identity: &str) -> Result<Self> {
        if !developer_id_application_identity.starts_with("Developer ID Application: ") {
            bail!(
                "'{developer_id_application_identity}' is not a 'Developer ID Application: …' \
                 identity: the carried packages are signed with the Developer ID and nothing else"
            );
        }
        Ok(Self {
            developer_id_application_identity: developer_id_application_identity.to_owned(),
        })
    }
}

impl MachOFileSigner for CodesignWithADeveloperIdApplicationIdentity {
    fn sign_the_mach_o_file(&self, mach_o_file: &Path, entitlements: Option<&Path>) -> Result<()> {
        let mut codesign = std::process::Command::new("codesign");
        codesign.args([
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--sign",
            &self.developer_id_application_identity,
        ]);
        if let Some(entitlements) = entitlements {
            codesign.arg("--entitlements").arg(entitlements);
        }
        let output = codesign
            .arg(mach_o_file)
            .output()
            .context("running `codesign`")?;
        if !output.status.success() {
            bail!(
                "`codesign` refused {}: {}",
                mach_o_file.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

/// The entitlements the carried Python's executables are signed with, relative to the workspace.
pub const CARRIED_PYTHON_ENTITLEMENTS_RELATIVE_TO_WORKSPACE: &str =
    "xtask/carried_python.entitlements";

/// Whether `leading_bytes`, a file's first bytes, open a Mach-O or a universal binary of them.
pub fn the_leading_bytes_open_a_mach_o(leading_bytes: &[u8]) -> bool {
    const MACH_O_AND_UNIVERSAL_MAGIC_NUMBERS: [[u8; 4]; 6] = [
        [0xfe, 0xed, 0xfa, 0xce],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0xca, 0xfe, 0xba, 0xbf],
    ];
    leading_bytes.len() >= 4
        && MACH_O_AND_UNIVERSAL_MAGIC_NUMBERS
            .iter()
            .any(|magic_number| leading_bytes[..4] == *magic_number)
}

/// A wheel `RECORD` hash: `sha256=` and the urlsafe base64 of the digest, unpadded.
fn the_wheel_record_hash_of(member_bytes: &[u8]) -> String {
    format!(
        "sha256={}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(member_bytes))
    )
}

/// One wheel member, as read.
struct WheelMemberRead {
    name: String,
    unix_mode: Option<u32>,
    is_a_directory: bool,
    compression_method: zip::CompressionMethod,
    member_bytes: Vec<u8>,
}

/// Sign every Mach-O member of `wheel` in place, rewrite its `RECORD` lines to their new sha256
/// and size, and return how many were signed. A wheel with none is left untouched.
pub fn sign_the_mach_o_inside_the_wheel(
    wheel: &Path,
    mach_o_file_signer: &dyn MachOFileSigner,
) -> Result<usize> {
    let mut wheel_members = read_the_wheel_members(wheel)?;
    let scratch = tempfile::tempdir().context("making a scratch directory")?;
    let mut signed_member_names = Vec::new();
    for (member_index, wheel_member) in wheel_members.iter_mut().enumerate() {
        if wheel_member.is_a_directory
            || !the_leading_bytes_open_a_mach_o(&wheel_member.member_bytes)
        {
            continue;
        }
        let member_file = scratch.path().join(format!("member-{member_index}"));
        std::fs::write(&member_file, &wheel_member.member_bytes)
            .with_context(|| format!("writing {}", member_file.display()))?;
        mach_o_file_signer
            .sign_the_mach_o_file(&member_file, None)
            .with_context(|| format!("signing {} in {}", wheel_member.name, wheel.display()))?;
        wheel_member.member_bytes = std::fs::read(&member_file)
            .with_context(|| format!("reading {}", member_file.display()))?;
        signed_member_names.push(wheel_member.name.clone());
    }
    if signed_member_names.is_empty() {
        return Ok(0);
    }
    rewrite_the_record_of_the_signed_members(wheel, &mut wheel_members, &signed_member_names)?;
    write_the_wheel_members(wheel, &wheel_members)?;
    Ok(signed_member_names.len())
}

fn read_the_wheel_members(wheel: &Path) -> Result<Vec<WheelMemberRead>> {
    let wheel_file =
        std::fs::File::open(wheel).with_context(|| format!("opening {}", wheel.display()))?;
    let mut wheel_archive = zip::ZipArchive::new(wheel_file)
        .with_context(|| format!("reading {} as a zip", wheel.display()))?;
    let mut wheel_members = Vec::with_capacity(wheel_archive.len());
    for member_index in 0..wheel_archive.len() {
        let mut wheel_member = wheel_archive
            .by_index(member_index)
            .with_context(|| format!("reading member {member_index} of {}", wheel.display()))?;
        let mut member_bytes = Vec::new();
        wheel_member
            .read_to_end(&mut member_bytes)
            .with_context(|| format!("reading {} in {}", wheel_member.name(), wheel.display()))?;
        wheel_members.push(WheelMemberRead {
            name: wheel_member.name().to_owned(),
            unix_mode: wheel_member.unix_mode(),
            is_a_directory: wheel_member.is_dir(),
            compression_method: wheel_member.compression(),
            member_bytes,
        });
    }
    Ok(wheel_members)
}

/// Each `RECORD` line is `path,hash,size`; a path may itself hold a comma, so the hash and the
/// size are the last two fields.
fn rewrite_the_record_of_the_signed_members(
    wheel: &Path,
    wheel_members: &mut [WheelMemberRead],
    signed_member_names: &[String],
) -> Result<()> {
    let signed_record_fields: Vec<(String, String, usize)> = wheel_members
        .iter()
        .filter(|wheel_member| signed_member_names.contains(&wheel_member.name))
        .map(|wheel_member| {
            (
                wheel_member.name.clone(),
                the_wheel_record_hash_of(&wheel_member.member_bytes),
                wheel_member.member_bytes.len(),
            )
        })
        .collect();
    let record_member = wheel_members
        .iter_mut()
        .find(|wheel_member| {
            wheel_member.name.ends_with(".dist-info/RECORD")
                && wheel_member.name.matches('/').count() == 1
        })
        .with_context(|| format!("{} carries no .dist-info/RECORD", wheel.display()))?;
    let record_text = String::from_utf8(std::mem::take(&mut record_member.member_bytes))
        .with_context(|| format!("{}'s RECORD is not UTF-8", wheel.display()))?;
    let mut rewritten_member_count = 0;
    let mut rewritten_record = String::with_capacity(record_text.len());
    for record_line in record_text.lines() {
        let record_path = record_line
            .rsplitn(3, ',')
            .nth(2)
            .map(|quoted_path| quoted_path.trim_matches('"'));
        match signed_record_fields
            .iter()
            .find(|(member_name, _, _)| Some(member_name.as_str()) == record_path)
        {
            Some((member_name, record_hash, member_size)) => {
                rewritten_record.push_str(&format!("{member_name},{record_hash},{member_size}\n"));
                rewritten_member_count += 1;
            }
            None => {
                rewritten_record.push_str(record_line);
                rewritten_record.push('\n');
            }
        }
    }
    if rewritten_member_count != signed_record_fields.len() {
        bail!(
            "{}'s RECORD lists {rewritten_member_count} of the {} Mach-O members signed",
            wheel.display(),
            signed_record_fields.len()
        );
    }
    record_member.member_bytes = rewritten_record.into_bytes();
    Ok(())
}

fn write_the_wheel_members(wheel: &Path, wheel_members: &[WheelMemberRead]) -> Result<()> {
    let partial_wheel = wheel.with_extension("whl.partial");
    let partial_wheel_file = std::fs::File::create(&partial_wheel)
        .with_context(|| format!("creating {}", partial_wheel.display()))?;
    let mut wheel_writer = zip::ZipWriter::new(partial_wheel_file);
    for wheel_member in wheel_members {
        let mut member_options = zip::write::SimpleFileOptions::default()
            .compression_method(wheel_member.compression_method)
            .large_file(wheel_member.member_bytes.len() >= u32::MAX as usize);
        if let Some(unix_mode) = wheel_member.unix_mode {
            member_options = member_options.unix_permissions(unix_mode & 0o7777);
        }
        if wheel_member.is_a_directory {
            wheel_writer
                .add_directory(wheel_member.name.as_str(), member_options)
                .with_context(|| format!("writing {} to {}", wheel_member.name, wheel.display()))?;
            continue;
        }
        wheel_writer
            .start_file(wheel_member.name.as_str(), member_options)
            .and_then(|()| Ok(wheel_writer.write_all(&wheel_member.member_bytes)?))
            .with_context(|| format!("writing {} to {}", wheel_member.name, wheel.display()))?;
    }
    wheel_writer
        .finish()
        .with_context(|| format!("finishing {}", partial_wheel.display()))?;
    std::fs::rename(&partial_wheel, wheel).with_context(|| {
        format!(
            "renaming {} to {}",
            partial_wheel.display(),
            wheel.display()
        )
    })
}

/// Sign every Mach-O in the CPython `python_archive` in place, each executable under
/// `python/bin/` with `executable_entitlements`, repack it, and return how many were signed.
pub fn sign_the_mach_o_inside_the_python_archive(
    python_archive: &Path,
    executable_entitlements: &Path,
    mach_o_file_signer: &dyn MachOFileSigner,
) -> Result<usize> {
    let scratch = tempfile::tempdir().context("making a scratch directory")?;
    run_tar(
        std::process::Command::new("tar")
            .arg("-xzf")
            .arg(python_archive)
            .arg("-C")
            .arg(scratch.path()),
    )?;
    let python_root = scratch.path().join("python");
    if !python_root.is_dir() {
        bail!(
            "{} holds no python/ directory, the layout uv installs",
            python_archive.display()
        );
    }
    let executables_directory = python_root.join("bin");
    let mut signed_mach_o_count = 0;
    for archive_entry in walkdir::WalkDir::new(&python_root).follow_links(false) {
        let archive_entry = archive_entry
            .with_context(|| format!("walking {} unpacked", python_archive.display()))?;
        if !archive_entry.file_type().is_file() || !the_file_opens_a_mach_o(archive_entry.path())? {
            continue;
        }
        let entitlements = archive_entry
            .path()
            .starts_with(&executables_directory)
            .then_some(executable_entitlements);
        mach_o_file_signer
            .sign_the_mach_o_file(archive_entry.path(), entitlements)
            .with_context(|| format!("signing a file of {}", python_archive.display()))?;
        signed_mach_o_count += 1;
    }
    let partial_archive = PathBuf::from(format!("{}.partial", python_archive.display()));
    // COPYFILE_DISABLE keeps macOS's tar from adding an AppleDouble `._` member for each
    // file that carries an extended attribute.
    run_tar(
        std::process::Command::new("tar")
            .env("COPYFILE_DISABLE", "1")
            .arg("-czf")
            .arg(&partial_archive)
            .arg("-C")
            .arg(scratch.path())
            .arg("python"),
    )?;
    std::fs::rename(&partial_archive, python_archive).with_context(|| {
        format!(
            "renaming {} to {}",
            partial_archive.display(),
            python_archive.display()
        )
    })?;
    Ok(signed_mach_o_count)
}

fn the_file_opens_a_mach_o(file: &Path) -> Result<bool> {
    let mut leading_bytes = [0_u8; 4];
    let mut opened =
        std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
    let leading_byte_count = opened
        .read(&mut leading_bytes)
        .with_context(|| format!("reading {}", file.display()))?;
    Ok(the_leading_bytes_open_a_mach_o(
        &leading_bytes[..leading_byte_count],
    ))
}

fn run_tar(tar: &mut std::process::Command) -> Result<()> {
    let output = tar.output().context("running `tar`")?;
    if !output.status.success() {
        bail!(
            "`tar` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// How uv's Python download metadata names the one CPython build the unit carries.
pub struct CarriedPythonDownloadForUv<'download> {
    /// The full version, `3.12.15`.
    pub version: &'download str,
    /// python-build-standalone's release, `20261013`.
    pub build: &'download str,
    /// What uv names the operating system: `darwin` or `linux`.
    pub uv_operating_system: &'download str,
    /// What uv names the architecture family: `aarch64` or `x86_64`.
    pub uv_architecture_family: &'download str,
    /// What uv names the C library: `none` on macOS, `gnu` on Linux.
    pub uv_libc: &'download str,
    /// The release URL, which uv reads below `UV_PYTHON_INSTALL_MIRROR` instead.
    pub url: &'download str,
    /// The sha256 of the archive the unit carries, as lowercase hex.
    pub sha256: &'download str,
}

/// Write `metadata_file`, the `UV_PYTHON_DOWNLOADS_JSON_URL` file naming the one CPython build the
/// unit carries with its sha256, which uv checks every Python download against, a mirrored one
/// included.
pub fn write_the_uv_python_download_metadata(
    metadata_file: &Path,
    carried_python: &CarriedPythonDownloadForUv<'_>,
) -> Result<()> {
    let version_numbers: Vec<u32> = carried_python
        .version
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("{} is no Python version", carried_python.version))?;
    let [major, minor, patch] = version_numbers[..] else {
        bail!("{} is no major.minor.patch version", carried_python.version);
    };
    let download_key = format!(
        "cpython-{}-{}-{}-{}",
        carried_python.version,
        if carried_python.uv_operating_system == "darwin" {
            "macos"
        } else {
            carried_python.uv_operating_system
        },
        carried_python.uv_architecture_family,
        carried_python.uv_libc
    );
    let metadata = serde_json::json!({
        download_key: {
            "name": "cpython",
            "arch": { "family": carried_python.uv_architecture_family, "variant": null },
            "os": carried_python.uv_operating_system,
            "libc": carried_python.uv_libc,
            "major": major,
            "minor": minor,
            "patch": patch,
            "prerelease": "",
            "url": carried_python.url,
            "sha256": carried_python.sha256,
            "variant": null,
            "build": carried_python.build,
        }
    });
    let metadata_text =
        serde_json::to_string_pretty(&metadata).context("writing uv's Python download metadata")?;
    std::fs::write(metadata_file, metadata_text)
        .with_context(|| format!("writing {}", metadata_file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Appends a fixed trailer to each file it signs, as `codesign` grows a Mach-O by its
    /// signature, and records what it signed with which entitlements.
    struct MachOFileSignerThatAppendsATrailer {
        signed_files: RefCell<Vec<(String, bool)>>,
    }

    const SIGNATURE_STAND_IN: &[u8] = b"signature stand-in";

    impl MachOFileSigner for MachOFileSignerThatAppendsATrailer {
        fn sign_the_mach_o_file(
            &self,
            mach_o_file: &Path,
            entitlements: Option<&Path>,
        ) -> Result<()> {
            let mut file_bytes = std::fs::read(mach_o_file)?;
            file_bytes.extend_from_slice(SIGNATURE_STAND_IN);
            std::fs::write(mach_o_file, file_bytes)?;
            self.signed_files.borrow_mut().push((
                mach_o_file
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                entitlements.is_some(),
            ));
            Ok(())
        }
    }

    fn a_mach_o_signer_that_appends_a_trailer() -> MachOFileSignerThatAppendsATrailer {
        MachOFileSignerThatAppendsATrailer {
            signed_files: RefCell::new(Vec::new()),
        }
    }

    const A_THIN_ARM64_MACH_O: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
    const A_PYTHON_SOURCE_FILE: &[u8] = b"import numpy\n";

    fn write_a_wheel(wheel: &Path, members: &[(&str, &[u8], u32)]) {
        let mut wheel_writer = zip::ZipWriter::new(std::fs::File::create(wheel).unwrap());
        let mut record = String::new();
        for (member_name, member_bytes, unix_mode) in members {
            wheel_writer
                .start_file(
                    *member_name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated)
                        .unix_permissions(*unix_mode),
                )
                .unwrap();
            wheel_writer.write_all(member_bytes).unwrap();
            record.push_str(&format!(
                "{member_name},{},{}\n",
                the_wheel_record_hash_of(member_bytes),
                member_bytes.len()
            ));
        }
        record.push_str("numpy-2.5.4.dist-info/RECORD,,\n");
        wheel_writer
            .start_file(
                "numpy-2.5.4.dist-info/RECORD",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        wheel_writer.write_all(record.as_bytes()).unwrap();
        wheel_writer.finish().unwrap();
    }

    fn the_member_of(wheel: &Path, member_name: &str) -> (Vec<u8>, Option<u32>) {
        let mut wheel_archive = zip::ZipArchive::new(std::fs::File::open(wheel).unwrap()).unwrap();
        let mut wheel_member = wheel_archive.by_name(member_name).unwrap();
        let mut member_bytes = Vec::new();
        wheel_member.read_to_end(&mut member_bytes).unwrap();
        (member_bytes, wheel_member.unix_mode())
    }

    #[test]
    fn a_wheels_mach_o_members_are_signed_and_its_record_names_their_new_hash_and_size() {
        let scratch = tempfile::tempdir().unwrap();
        let wheel = scratch
            .path()
            .join("numpy-2.5.4-cp312-cp312-macosx_14_0_arm64.whl");
        write_a_wheel(
            &wheel,
            &[
                ("numpy/__init__.py", A_PYTHON_SOURCE_FILE, 0o644),
                (
                    "numpy/_core/_multiarray_umath.cpython-312-darwin.so",
                    A_THIN_ARM64_MACH_O,
                    0o755,
                ),
                (
                    "numpy/.dylibs/libopenblas64_.0.dylib",
                    A_THIN_ARM64_MACH_O,
                    0o644,
                ),
            ],
        );
        let mach_o_file_signer = a_mach_o_signer_that_appends_a_trailer();

        let signed_mach_o_count =
            sign_the_mach_o_inside_the_wheel(&wheel, &mach_o_file_signer).unwrap();

        assert_eq!(signed_mach_o_count, 2);
        let signed_extension_bytes = [A_THIN_ARM64_MACH_O, SIGNATURE_STAND_IN].concat();
        let (extension_bytes, extension_mode) = the_member_of(
            &wheel,
            "numpy/_core/_multiarray_umath.cpython-312-darwin.so",
        );
        assert_eq!(extension_bytes, signed_extension_bytes);
        assert_eq!(extension_mode.map(|mode| mode & 0o777), Some(0o755));
        assert_eq!(
            the_member_of(&wheel, "numpy/__init__.py").0,
            A_PYTHON_SOURCE_FILE
        );
        let record =
            String::from_utf8(the_member_of(&wheel, "numpy-2.5.4.dist-info/RECORD").0).unwrap();
        assert_eq!(
            record,
            format!(
                "numpy/__init__.py,{},{}\n\
                 numpy/_core/_multiarray_umath.cpython-312-darwin.so,{},{}\n\
                 numpy/.dylibs/libopenblas64_.0.dylib,{},{}\n\
                 numpy-2.5.4.dist-info/RECORD,,\n",
                the_wheel_record_hash_of(A_PYTHON_SOURCE_FILE),
                A_PYTHON_SOURCE_FILE.len(),
                the_wheel_record_hash_of(&signed_extension_bytes),
                signed_extension_bytes.len(),
                the_wheel_record_hash_of(&signed_extension_bytes),
                signed_extension_bytes.len(),
            )
        );
        assert!(
            mach_o_file_signer
                .signed_files
                .borrow()
                .iter()
                .all(|(_, signed_with_entitlements)| !signed_with_entitlements)
        );
    }

    #[test]
    fn a_wheel_with_no_mach_o_is_left_byte_for_byte() {
        let scratch = tempfile::tempdir().unwrap();
        let wheel = scratch.path().join("msgpack-1.1.0-py3-none-any.whl");
        write_a_wheel(
            &wheel,
            &[("msgpack/__init__.py", A_PYTHON_SOURCE_FILE, 0o644)],
        );
        let wheel_bytes_before = std::fs::read(&wheel).unwrap();

        let signed_mach_o_count =
            sign_the_mach_o_inside_the_wheel(&wheel, &a_mach_o_signer_that_appends_a_trailer())
                .unwrap();

        assert_eq!(signed_mach_o_count, 0);
        assert_eq!(std::fs::read(&wheel).unwrap(), wheel_bytes_before);
    }

    #[test]
    fn the_python_archives_mach_o_are_signed_its_executables_with_the_entitlements() {
        let scratch = tempfile::tempdir().unwrap();
        let unpacked = scratch.path().join("unpacked");
        std::fs::create_dir_all(unpacked.join("python/bin")).unwrap();
        std::fs::create_dir_all(unpacked.join("python/lib/python3.12/lib-dynload")).unwrap();
        std::fs::write(unpacked.join("python/bin/python3.12"), A_THIN_ARM64_MACH_O).unwrap();
        std::os::unix::fs::symlink("python3.12", unpacked.join("python/bin/python3")).unwrap();
        std::fs::write(
            unpacked.join("python/lib/python3.12/lib-dynload/_ssl.cpython-312-darwin.so"),
            A_THIN_ARM64_MACH_O,
        )
        .unwrap();
        std::fs::write(
            unpacked.join("python/lib/python3.12/os.py"),
            A_PYTHON_SOURCE_FILE,
        )
        .unwrap();
        let python_archive = scratch
            .path()
            .join("cpython-3.12.15+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz");
        run_tar(
            std::process::Command::new("tar")
                .arg("-czf")
                .arg(&python_archive)
                .arg("-C")
                .arg(&unpacked)
                .arg("python"),
        )
        .unwrap();
        let mach_o_file_signer = a_mach_o_signer_that_appends_a_trailer();

        let signed_mach_o_count = sign_the_mach_o_inside_the_python_archive(
            &python_archive,
            Path::new("carried_python.entitlements"),
            &mach_o_file_signer,
        )
        .unwrap();

        assert_eq!(signed_mach_o_count, 2);
        let mut signed_files = mach_o_file_signer.signed_files.borrow().clone();
        signed_files.sort();
        assert_eq!(
            signed_files,
            [
                ("_ssl.cpython-312-darwin.so".to_owned(), false),
                ("python3.12".to_owned(), true),
            ]
        );
        let repacked = scratch.path().join("repacked");
        std::fs::create_dir_all(&repacked).unwrap();
        run_tar(
            std::process::Command::new("tar")
                .arg("-xzf")
                .arg(&python_archive)
                .arg("-C")
                .arg(&repacked),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(repacked.join("python/bin/python3.12")).unwrap(),
            [A_THIN_ARM64_MACH_O, SIGNATURE_STAND_IN].concat()
        );
        assert_eq!(
            std::fs::read_link(repacked.join("python/bin/python3")).unwrap(),
            Path::new("python3.12")
        );
        assert_eq!(
            std::fs::read(repacked.join("python/lib/python3.12/os.py")).unwrap(),
            A_PYTHON_SOURCE_FILE
        );
    }

    #[test]
    fn only_a_developer_id_application_identity_signs_the_carried_packages() {
        assert!(CodesignWithADeveloperIdApplicationIdentity::for_the_identity("-").is_err());
        assert!(
            CodesignWithADeveloperIdApplicationIdentity::for_the_identity(
                "Apple Development: someone (TEAMID1234)"
            )
            .is_err()
        );
        assert!(
            CodesignWithADeveloperIdApplicationIdentity::for_the_identity(
                "Developer ID Application: Tatolab (TEAMID1234)"
            )
            .is_ok()
        );
    }

    #[test]
    fn the_uv_metadata_names_the_carried_build_by_uvs_key_and_its_sha256() {
        let scratch = tempfile::tempdir().unwrap();
        let metadata_file = scratch.path().join("uv-python-downloads.json");

        write_the_uv_python_download_metadata(
            &metadata_file,
            &CarriedPythonDownloadForUv {
                version: "3.12.15",
                build: "20261013",
                uv_operating_system: "darwin",
                uv_architecture_family: "aarch64",
                uv_libc: "none",
                url: "https://github.com/astral-sh/python-build-standalone/releases/download/\
                      20261013/cpython-3.12.15%2B20261013-aarch64-apple-darwin-install_only_stripped.tar.gz",
                sha256: "9866e11cf2ae9caa1e671383786f248859a52dd8bfb702fb4c0dc2c65f1d36de",
            },
        )
        .unwrap();

        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata_file).unwrap()).unwrap();
        let carried = &metadata["cpython-3.12.15-macos-aarch64-none"];
        assert_eq!(carried["os"], "darwin");
        assert_eq!(carried["arch"]["family"], "aarch64");
        assert_eq!(carried["libc"], "none");
        assert_eq!(
            (&carried["major"], &carried["minor"], &carried["patch"]),
            (
                &serde_json::json!(3),
                &serde_json::json!(12),
                &serde_json::json!(15)
            )
        );
        assert_eq!(
            carried["sha256"],
            "9866e11cf2ae9caa1e671383786f248859a52dd8bfb702fb4c0dc2c65f1d36de"
        );
        assert_eq!(carried["build"], "20261013");
    }

    #[test]
    fn a_universal_binary_is_a_mach_o_and_a_text_file_is_not() {
        assert!(the_leading_bytes_open_a_mach_o(&[
            0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2
        ]));
        assert!(the_leading_bytes_open_a_mach_o(A_THIN_ARM64_MACH_O));
        assert!(!the_leading_bytes_open_a_mach_o(A_PYTHON_SOURCE_FILE));
        assert!(!the_leading_bytes_open_a_mach_o(&[0xcf, 0xfa]));
    }
}
