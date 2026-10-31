# zenteru shell integration bootstrap (zsh).
# zenteru starts zsh with ZDOTDIR pointing here; restore the user's ZDOTDIR
# first so their .zprofile / .zshrc / .zlogin load exactly as usual.
if [[ -n "${ZENTERU_ORIG_ZDOTDIR-}" ]]; then
  ZDOTDIR="$ZENTERU_ORIG_ZDOTDIR"
else
  unset ZDOTDIR
fi
unset ZENTERU_ORIG_ZDOTDIR
[[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]] && builtin source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive && -r "${ZENTERU_SHELL_DIR-}/zenteru.zsh" ]]; then
  builtin source "$ZENTERU_SHELL_DIR/zenteru.zsh"
fi
