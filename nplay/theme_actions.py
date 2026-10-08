"""Desktop integration for custom theme files; no shell or theme execution."""
import os
from pathlib import Path
import shutil
import subprocess

def open_theme_directory(directory: Path):
    """Open the XDG theme directory, without invoking a shell."""
    try:
        directory.mkdir(parents=True, exist_ok=True)
        if directory.is_symlink() or not directory.is_dir():
            return False, "Theme directory is not a regular directory"
        opener = shutil.which("xdg-open")
        if not opener:
            return False, f"No desktop file opener found · {directory}"
        with open(os.devnull, "wb") as null:
            subprocess.Popen([opener, str(directory)], stdin=null,
                             stdout=null, stderr=null, start_new_session=True)
        return True, "Theme directory opened"
    except (OSError, ValueError) as exc:
        return False, f"Cannot open theme directory · {exc}"
