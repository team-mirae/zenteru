"""Entry point of the Tab-completion plugin (see complete/app.py)."""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from complete.app import main  # noqa: E402

if __name__ == "__main__":
    main()
