//! Help and usage texts, identical to the 80-column output of the original
//! argparse-based CLI run under the program name `moldbox`.

pub(crate) const TOP_USAGE: &str = "usage: moldbox [-h] [--version] [--json]
               {checkpoint,cp,backup,bk,snapshot,snap,info} ...
";

pub(crate) const TOP_HELP: &str = "usage: moldbox [-h] [--version] [--json]
               {checkpoint,cp,backup,bk,snapshot,snap,info} ...

Atomic SQLite backup & checkpoint tool.

positional arguments:
  {checkpoint,cp,backup,bk,snapshot,snap,info}
    checkpoint (cp)     Run a WAL checkpoint
    backup (bk)         Create an atomic backup
    snapshot (snap)     Checkpoint then backup (recommended)
    info                Show database info

options:
  -h, --help            show this help message and exit
  --version             show program's version number and exit
  --json                Output results as JSON
";

pub(crate) const CHECKPOINT_USAGE: &str =
    "usage: moldbox checkpoint [-h] [-m {passive,full,restart,truncate}]
                          [--require-complete]
                          database
";

pub(crate) const CHECKPOINT_HELP: &str =
    "usage: moldbox checkpoint [-h] [-m {passive,full,restart,truncate}]
                          [--require-complete]
                          database

positional arguments:
  database              Path to the SQLite database

options:
  -h, --help            show this help message and exit
  -m, --mode {passive,full,restart,truncate}
                        Checkpoint mode (default: passive)
  --require-complete    Exit 3 when the checkpoint is busy or partial
";

pub(crate) const BACKUP_USAGE: &str = "usage: moldbox backup [-h] [--pages-per-step PAGES_PER_STEP]
                      [--pause-ms PAUSE_MS]
                      database destination
";

pub(crate) const BACKUP_HELP: &str = "usage: moldbox backup [-h] [--pages-per-step PAGES_PER_STEP]
                      [--pause-ms PAUSE_MS]
                      database destination

positional arguments:
  database              Path to the SQLite database
  destination           Path for the backup file

options:
  -h, --help            show this help message and exit
  --pages-per-step PAGES_PER_STEP
                        Positive pages to copy per backup step (default: 1000)
  --pause-ms PAUSE_MS   Milliseconds to pause between backup steps (default:
                        0)
";

pub(crate) const SNAPSHOT_USAGE: &str =
    "usage: moldbox snapshot [-h] [-m {passive,full,restart,truncate}]
                        [--pages-per-step PAGES_PER_STEP]
                        [--pause-ms PAUSE_MS] [--require-complete]
                        database destination
";

pub(crate) const SNAPSHOT_HELP: &str =
    "usage: moldbox snapshot [-h] [-m {passive,full,restart,truncate}]
                        [--pages-per-step PAGES_PER_STEP]
                        [--pause-ms PAUSE_MS] [--require-complete]
                        database destination

positional arguments:
  database              Path to the SQLite database
  destination           Path for the backup file

options:
  -h, --help            show this help message and exit
  -m, --mode {passive,full,restart,truncate}
                        Checkpoint mode (default: passive)
  --pages-per-step PAGES_PER_STEP
                        Positive pages to copy per backup step (default: 1000)
  --pause-ms PAUSE_MS   Milliseconds to pause between backup steps (default:
                        0)
  --require-complete    Exit 3 when the checkpoint is busy or partial
";

pub(crate) const INFO_USAGE: &str = "usage: moldbox info [-h] database
";

pub(crate) const INFO_HELP: &str = "usage: moldbox info [-h] database

positional arguments:
  database    Path to the SQLite database

options:
  -h, --help  show this help message and exit
";
