"""Publish the repository's canonical llms.txt at the documentation root."""

from pathlib import Path
import shutil


def on_post_build(config, **kwargs) -> None:  # noqa: ANN001, ANN003
    """Copy the canonical source after MkDocs has prepared the site directory."""
    source = Path(config.config_file_path).resolve().parent / "llms.txt"
    destination = Path(config.site_dir) / "llms.txt"
    shutil.copyfile(source, destination)
