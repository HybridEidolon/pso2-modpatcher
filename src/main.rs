use ages_ice_archive::{Group, IceArchive, IceGroupIter, IceWriter};

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicU64, Ordering}, mpsc};

use anyhow::{bail, Context};
use ascii::{AsciiStr, AsciiString};
use structopt::StructOpt;

#[cfg(windows)]
use nwg::NativeUi;

#[cfg(windows)]
use nwd::NwgUi;


#[cfg(windows)]
#[derive(Default, NwgUi)]
pub struct PatcherApp {
    #[nwg_control(size: (300, 120), title: "Patcher", flags: "VISIBLE")]
    #[nwg_events( OnWindowClose: [PatcherApp::on_quit] )]
    window: nwg::Window,

    #[nwg_control(parent: window)]
    #[nwg_events( OnNotice: [PatcherApp::on_notice] )]
    notice: nwg::Notice,

    #[nwg_layout(parent: window, spacing: 2, margin: [8, 8, 8, 8], min_size: [150, 60])]
    layout: nwg::GridLayout,

    #[nwg_control(text: "Patching...")]
    #[nwg_layout_item(layout: layout, col: 0, row: 0, col_span: 5)]
    label: nwg::Label,

    #[nwg_control(text: "0 files patched")]
    #[nwg_layout_item(layout: layout, col: 0, row: 1, col_span: 5)]
    progress_label: nwg::Label,

    patched_files: AtomicU64,
}

#[cfg(windows)]
impl PatcherApp {
    fn on_quit(&self) {
        nwg::stop_thread_dispatch();
    }

    fn on_notice(&self) {
        let patched_files = self.patched_files.fetch_add(1, Ordering::Relaxed) + 1;
        self.progress_label.set_text(&format!("{} files patched", patched_files));
    }
}

#[cfg(windows)]
pub enum PatcherEvent {
    Progress,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "pso2-modpatcher", about = "Tool for repacking ICE archives in a directory with new files")]
struct Args {
    #[structopt(parse(from_os_str), help = "Patch path to apply")]
    input: PathBuf,

    #[structopt(parse(from_os_str), help = "Data directory to patch")]
    datadir: PathBuf,

    #[structopt(long = "verbose", short = "v", help = "Print additional work information to stderr")]
    verbose: bool,

    #[structopt(long = "no-backup", help = "Don't create a backup of the patched files")]
    no_backup: bool,

    #[cfg(windows)]
    #[structopt(long = "gui", help = "Show a gui window during patching instead of a console (Windows only)")]
    gui: bool,
}

fn iterate_patch_directory(src: &Path, out: &Path, backup_path: Option<&Path>, verbose: bool, events: mpsc::Sender<PatcherEvent>) -> anyhow::Result<()> {
    if !src.is_dir() {
        panic!("src is not a directory");
    }
    if !out.is_dir() {
        panic!("out is not a directory");
    }
    if let Some(backup_path) = backup_path {
        if backup_path.exists() && !backup_path.is_dir() {
            panic!("backup path is not a directory");
        }
        if !backup_path.exists() {
            std::fs::create_dir_all(backup_path)
                .with_context(|| "Failed to make backup directory")?;
        }
    }

    if verbose {
        eprintln!("Working on patch source directory {}", src.to_string_lossy());
    }

    let read_dir = src.read_dir().with_context(|| format!("Failed to iterate over patch directory {}", src.to_string_lossy()))?;
    for file in read_dir {
        let file_entry = file.with_context(|| format!("Failed to index a file in patch directory {}", src.to_string_lossy()))?;
        
        let file_entry_path = file_entry.path();
        if file_entry_path.is_dir() {
            let file_name = file_entry_path.file_name().unwrap();
            let file_name_lossy = file_name.to_string_lossy();
            if file_name_lossy == "backup" {
                bail!("File name of a patch directory in {} is \"backup\", which is not allowed", src.to_string_lossy());
            }
            if file_name_lossy.ends_with("_ice") {
                // this is an ice file to patch
                let ice_out = out.join(file_name_lossy.strip_suffix("_ice").unwrap());
                let backup_file = backup_path.map(|p| p.join(file_name_lossy.strip_suffix("_ice").unwrap()));

                match apply_directory(&file_entry_path, &ice_out, backup_file.as_ref().map(|p| p.as_path()), verbose, events.clone())
                    .with_context(|| format!("Failed to patch ICE file {}", ice_out.to_string_lossy())) {
                    Err(e) => {
                        eprintln!("{:?}\nContinuing...", e);
                    },
                    _ => {},
                }
            } else {
                // this is another directory to iterate
                let out_path = out.join(file_name);
                let next_backup_path = backup_path.map(|p| p.join(file_name));

                match iterate_patch_directory(&file_entry_path, &out_path, next_backup_path.as_ref().map(|p| p.as_path()), verbose, events.clone())
                    .with_context(|| format!("Failed to apply directory {}", out_path.to_string_lossy())) {
                    Err(e) => {
                        eprintln!("{:?}\nContinuing...", e);
                    },
                    _ => {},
                }
            }
        }
    }

    Ok(())
}

fn apply_directory(patch_src: &Path, out_file: &Path, backup_file: Option<&Path>, verbose: bool, events: mpsc::Sender<PatcherEvent>) -> anyhow::Result<()> {
    // The patch_src is assumed to contain two directories, 1 and 2
    // Each correspond to a group in the out_file ICE to replace files in

    // these are required invariants to this function
    if !patch_src.is_dir() {
        panic!("patch src was not a directory");
    }

    if !out_file.exists() {
        // not a failure, but we can't apply this patch
        eprintln!("{} missing; skipping", out_file.to_string_lossy());
        return Ok(());
    }

    if !out_file.is_file() {
        panic!("out file is not a file");
    }

    let mut src_1 = patch_src.to_path_buf();
    src_1.push("1");
    let mut src_2 = patch_src.to_path_buf();
    src_2.push("2");

    if src_1.exists() && !src_1.is_dir() {
        bail!("1 in patch directory {} is not a directory", patch_src.to_string_lossy());
    }
    if src_2.exists() && !src_2.is_dir() {
        bail!("2 in patch directory {} is not a directory", patch_src.to_string_lossy());
    }
    if !src_1.exists() && !src_2.exists() {
        bail!("Patch directory {} does not contain any files to patch", patch_src.to_string_lossy());
    }

    if verbose {
        eprintln!("Patching ICE file {}", out_file.to_string_lossy());
    }

    let orig_ia_file = File::open(out_file)
        .with_context(|| format!("Failed to open target ICE file \"{}\"", out_file.to_string_lossy()))?;
    let orig_ia = IceArchive::load(orig_ia_file)
        .with_context(|| format!(
            "Failed to load \"{}\" as an ICE",
            out_file.to_string_lossy(),
        ))?;

    if let Some(backup_file) = backup_file {
        if !backup_file.exists() {
            if let Some(_backup_parent) = backup_file.parent() {
                if verbose {
                    eprintln!("Backing up {} to {}", out_file.to_string_lossy(), backup_file.to_string_lossy());
                }
                std::fs::rename(out_file, backup_file)
                    .with_context(|| format!(
                        "Failed to copy the target ICE file {} to the backup path {}",
                        out_file.to_string_lossy(),
                        backup_file.to_string_lossy(),
                    ))?;
            } else {
                panic!("backup path parent does not exist");
            }
        } else {
            eprintln!("Backup file {} exists; not replacing it with a new backup", backup_file.to_string_lossy());
        }
    }
    
    if orig_ia.version() != 4 {
        bail!(
            "Unable to patch ICE file {} with version {}",
            out_file.to_string_lossy(),
            orig_ia.version(),
        );
    }

    // compression is disabled because the kraken encoder is broken :(
    // let compress = (orig_ia.is_compressed(Group::Group1) || orig_ia.is_compressed(Group::Group2)) && orig_ia.is_oodle();
    let compress = false;
    let encrypt = orig_ia.is_encrypted();
    let oodle = (orig_ia.is_compressed(Group::Group1) || orig_ia.is_compressed(Group::Group2)) && orig_ia.is_oodle();
    
    let mut new_ia = IceWriter::new(4, compress, encrypt, oodle)
        .with_context(|| "Unable to start creating new ICE archive")?;
    
    let orig_g1_data = orig_ia.decompress_group(Group::Group1)
        .with_context(|| format!(
            "Failed to unpack group 1 of {}",
            out_file.to_string_lossy(),
        ))?;
    let orig_g2_data = orig_ia.decompress_group(Group::Group2)
        .with_context(|| format!(
            "Failed to unpack group 2 of {}",
            out_file.to_string_lossy(),
        ))?;
    
    let orig_g1_files_iter: IceGroupIter = match IceGroupIter::new(&orig_g1_data[..], orig_ia.group_count(Group::Group1)) {
        Ok(i) => i,
        Err(_) => bail!(
            "Unable to iterate over group 1 files in {}",
            out_file.to_string_lossy(),
        ),
    };

    let mut g1_added_files: HashSet<String> = HashSet::new();
    for file in orig_g1_files_iter {
        // unwrap here as these don't have std errors yet and it is exceedingly
        // unlikely to find a malformed ICE archive at this point
        let ext = file.ext().unwrap();
        let name = file.name().unwrap();
        let data = file.data();

        let name_ascii = unsafe { AsciiStr::from_ascii_unchecked(name.as_bytes()) };
        let ext_ascii = unsafe { AsciiStr::from_ascii_unchecked(ext.as_bytes()) };

        let replacer_path = src_1.join(name);
        if replacer_path.exists() {
            if !replacer_path.is_file() {
                bail!(
                    "Replacement path {} for group 1 of {} is not a file",
                    replacer_path.to_string_lossy(),
                    out_file.to_string_lossy(),
                );
            }

            let replacer_file = std::fs::read(&replacer_path)
                .with_context(|| format!(
                    "Failed to open replacement file {} for group 1 of {}",
                    replacer_path.to_string_lossy(),
                    out_file.to_string_lossy(),
                ))?;
            
            let mut of = new_ia.begin_file(name_ascii, ext_ascii, Group::Group1);
            of
                .write_all(&replacer_file[..])
                .with_context(|| format!(
                    "Failed to write replacement {} in group 1 of {}",
                    replacer_path.to_string_lossy(),
                    out_file.to_string_lossy(),
                ))?;
            of.finish();
            g1_added_files.insert(name.to_owned());
        } else {
            let mut of = new_ia.begin_file(name_ascii, ext_ascii, Group::Group1);
            of
                .write_all(&data[..])
                .with_context(|| format!(
                    "Failed to write {} in group 1 of {}",
                    name,
                    out_file.to_string_lossy(),
                ))?;
            of.finish();
            g1_added_files.insert(name.to_owned());
        }
    }

    if src_1.exists() {
        for file in src_1.read_dir().with_context(|| format!("Unable to read dir {} for adding files to {}", src_1.to_string_lossy(), out_file.to_string_lossy()))? {
            let file = file.with_context(|| format!(
                "Unable to index file while reading dir {} for adding files to {}",
                src_1.to_string_lossy(),
                out_file.to_string_lossy(),
            ))?;

            let file_name_string = file.file_name().to_string_lossy().into_owned();
            if !g1_added_files.contains(&file_name_string) {
                let ascii_name = AsciiString::from_ascii(file_name_string.as_bytes().to_owned())
                    .with_context(|| format!(
                        "File name of {} is not valid ASCII",
                        file.path().to_string_lossy(),
                    ))?;
                let ascii_ext = match file.path().extension() {
                    Some(e) => {
                        let e_owned = e.to_string_lossy().into_owned();
                        AsciiString::from_ascii(e_owned.as_bytes().to_owned()).with_context(|| format!(
                            "File extension of {} is not valid ASCII",
                            file.path().to_string_lossy(),
                        ))?.to_owned()
                    },
                    None => bail!("File {} has no extension", file.path().to_string_lossy()),
                };
                let fc = std::fs::read(file.path())
                    .with_context(|| format!(
                        "Unable to read contents of file {}",
                        file.path().to_string_lossy(),
                    ))?;
                let mut of = new_ia.begin_file(&ascii_name, &ascii_ext, Group::Group1);
                of.write_all(&fc[..])
                    .with_context(|| format!(
                        "Unable to write contents of file {} to ICE file writer",
                        file.path().to_string_lossy(),
                    ))?;
                of.finish();
                g1_added_files.insert(file_name_string);
            }
        }
    }

    let orig_g2_files_iter: IceGroupIter = match IceGroupIter::new(&orig_g2_data[..], orig_ia.group_count(Group::Group2)) {
        Ok(i) => i,
        Err(_) => bail!(
            "Unable to iterate over group 2 files in {}",
            out_file.to_string_lossy(),
        ),
    };

    let mut g2_added_files: HashSet<String> = HashSet::new();
    for file in orig_g2_files_iter {
        // unwrap here as these don't have std errors yet and it is exceedingly
        // unlikely to find a malformed ICE archive at this point
        let ext = file.ext().unwrap();
        let name = file.name().unwrap();
        let data = file.data();

        let name_ascii = unsafe { AsciiStr::from_ascii_unchecked(name.as_bytes()) };
        let ext_ascii = unsafe { AsciiStr::from_ascii_unchecked(ext.as_bytes()) };

        let replacer_path = src_2.join(name);
        if replacer_path.exists() {
            if !replacer_path.is_file() {
                bail!(
                    "Replacement path {} for group 2 of {} is not a file",
                    replacer_path.to_string_lossy(),
                    out_file.to_string_lossy(),
                );
            }

            let replacer_file = std::fs::read(&replacer_path)
                .with_context(|| format!(
                    "Failed to open replacement file {} for group 2 of {}",
                    replacer_path.to_string_lossy(),
                    out_file.to_string_lossy(),
                ))?;
            
            let mut of = new_ia.begin_file(name_ascii, ext_ascii, Group::Group2);
            of
                .write_all(&replacer_file[..])
                .with_context(|| format!(
                    "Failed to write replacement {} in group 2 of {}",
                    replacer_path.to_string_lossy(),
                    out_file.to_string_lossy(),
                ))?;
            of.finish();
            g2_added_files.insert(name.to_owned());
        } else {
            let mut of = new_ia.begin_file(name_ascii, ext_ascii, Group::Group2);
            of
                .write_all(&data[..])
                .with_context(|| format!(
                    "Failed to write {} in group 2 of {}",
                    name,
                    out_file.to_string_lossy(),
                ))?;
            of.finish();
            g2_added_files.insert(name.to_owned());
        }
    }

    if src_2.exists() {
        for file in src_2.read_dir().with_context(|| format!("Unable to read dir {} for adding files to {}", src_2.to_string_lossy(), out_file.to_string_lossy()))? {
            let file = file.with_context(|| format!(
                "Unable to index file while reading dir {} for adding files to {}",
                src_2.to_string_lossy(),
                out_file.to_string_lossy(),
            ))?;

            let file_name_string = file.file_name().to_string_lossy().into_owned();
            if !g2_added_files.contains(&file_name_string) {
                let ascii_name = AsciiString::from_ascii(file_name_string.as_bytes().to_owned())
                    .with_context(|| format!(
                        "File name of {} is not valid ASCII",
                        file.path().to_string_lossy(),
                    ))?;
                let ascii_ext = match file.path().extension() {
                    Some(e) => {
                        let e_owned = e.to_string_lossy().into_owned();
                        AsciiString::from_ascii(e_owned.as_bytes().to_owned()).with_context(|| format!(
                            "File extension of {} is not valid ASCII",
                            file.path().to_string_lossy(),
                        ))?.to_owned()
                    },
                    None => bail!("File {} has no extension", file.path().to_string_lossy()),
                };
                let fc = std::fs::read(file.path())
                    .with_context(|| format!(
                        "Unable to read contents of file {}",
                        file.path().to_string_lossy(),
                    ))?;
                let mut of = new_ia.begin_file(&ascii_name, &ascii_ext, Group::Group2);
                of.write_all(&fc[..])
                    .with_context(|| format!(
                        "Unable to write contents of file {} to ICE file writer",
                        file.path().to_string_lossy(),
                    ))?;
                of.finish();
                g2_added_files.insert(file_name_string);
            }
        }
    }

    let new_ia_file = File::create(out_file)
        .with_context(|| format!(
            "Unable to open ICE file path {} for writing patched archive from {}",
            out_file.to_string_lossy(),
            patch_src.to_string_lossy(),
        ))?;
    
    new_ia.finish(new_ia_file)
        .with_context(|| format!(
            "Unable to write patched ICE archive to {}",
            out_file.to_string_lossy(),
        ))?;

    // event sender is allowed to fail (for no receivers)
    let _e = events.send(PatcherEvent::Progress);

    Ok(())
}

fn main() {
    let args = Args::from_args();

    if !args.input.exists() {
        eprintln!("pso2-modpatcher: input patch not found");
        std::process::exit(1);
    }
    if args.input.is_file() {
        eprintln!("pso2-modpatcher: input patch is a file");
        std::process::exit(1);
    }
    if !args.datadir.exists() {
        eprintln!("pso2-modpatcher: output data path does not exist");
        std::process::exit(1);
    }
    if args.datadir.is_file() {
        eprintln!("pso2-modpatcher: output data path is a file");
        std::process::exit(1);
    }

    let (tx, rx) = mpsc::channel::<PatcherEvent>();

    #[cfg(windows)]
    if args.gui {
        unsafe { winapi::um::wincon::FreeConsole(); }
        std::thread::spawn(move || {
            nwg::init().unwrap();
            nwg::Font::set_global_family("Segoe UI").unwrap();
            let gui = PatcherApp::build_ui(Default::default()).unwrap();
            let notice_sender = gui.notice.sender();
            std::thread::spawn(move || {
                loop {
                    let _evt = match rx.recv() {
                        Ok(e) => e,
                        Err(_e) => break,
                    };
                    notice_sender.notice();
                }
            });
            nwg::dispatch_thread_events();
        });
    } else {
        drop(rx);
    }

    let backup_dir;
    if args.no_backup {
        backup_dir = None;
    } else {
        backup_dir = Some(args.datadir.join("backup"));
    }

    // apply_directory(&args.input, &args.datadir)?;
    match iterate_patch_directory(&args.input, &args.datadir, backup_dir.as_ref().map(|v| v.as_path()), args.verbose, tx.clone()) {
        Ok(_) => {},
        Err(e) => {
            
            eprintln!("pso2-modpatcher: {}", e);
            return;
        },
    }
}
