# zenteru shell integration for zsh.
#
#  * OSC 133 A/B/C/D prompt marks (FinalTerm), OSC 633;E command line and
#    OSC 7 working directory, so zenteru knows where the command line is
#    (natural ⌥ editing, Tab routing, ⌘↑/⌘↓ jumps, plugin prompt events).
#  * setenvvar: variables set for this session through the plugin API are
#    exported just before each command you run and removed again after it,
#    so they never sit in the prompt's environment or in history, and are
#    skipped for commands sent by plugins.
#  * Tab-completion capture for the completion plugin (complete/).

[[ -n "${_ZENTERU_LOADED-}" ]] && return
typeset -g _ZENTERU_LOADED=1
autoload -Uz add-zsh-hook add-zle-hook-widget 2>/dev/null

typeset -g _zt_running=""
typeset -gA _zt_saved
typeset -ga _zt_applied

_zt_osc() { builtin printf '\e]%s\a' "$1" >/dev/tty 2>/dev/null; }

_zenteru_env_apply() {
  local d="${ZENTERU_SESSION_DIR-}" n
  _zt_applied=()
  [[ -z "$d" ]] && return
  if [[ -e "$d/skip-env-once" ]]; then
    command rm -f -- "$d/skip-env-once"
    return
  fi
  [[ -r "$d/env.names" && -r "$d/env" ]] || return
  for n in ${(f)"$(<"$d/env.names")"}; do
    [[ -z "$n" ]] && continue
    if (( ${+parameters[$n]} )); then _zt_saved[$n]="${(P)n}"; fi
    _zt_applied+=("$n")
  done
  builtin source "$d/env"
}

_zenteru_env_restore() {
  local n
  for n in "${_zt_applied[@]}"; do
    if (( ${+_zt_saved[$n]} )); then
      typeset -gx "$n=${_zt_saved[$n]}"
    else
      unset "$n"
    fi
  done
  _zt_applied=()
  _zt_saved=()
}

_zenteru_precmd() {
  local ret=$?
  if [[ -n "$_zt_running" ]]; then
    _zt_osc "133;D;$ret"
    _zt_running=""
  fi
  _zenteru_env_restore
  _zt_osc "7;file://${HOST}${PWD// /%20}"
  _zt_osc "133;A"
}

_zenteru_preexec() {
  local cmd="${1//\\/\\\\}"
  cmd="${cmd//;/\\x3b}"
  cmd="${cmd//$'\n'/\\x0a}"
  _zt_osc "633;E;${cmd}"
  _zt_osc "133;C"
  _zt_running=1
  _zenteru_env_apply
}

_zenteru_line_init() { _zt_osc "133;B"; }

# ZLE isn't up yet while .zshenv runs, so hook line-init on the first prompt.
_zenteru_first_precmd() {
  add-zsh-hook -d precmd _zenteru_first_precmd
  if (( $+functions[add-zle-hook-widget] )); then
    add-zle-hook-widget line-init _zenteru_line_init
  else
    zle -N zle-line-init _zenteru_line_init
  fi
}

add-zsh-hook precmd _zenteru_precmd
add-zsh-hook precmd _zenteru_first_precmd
add-zsh-hook preexec _zenteru_preexec

[[ -r "$ZENTERU_SHELL_DIR/complete/zenteru-complete.zsh" ]] && builtin source "$ZENTERU_SHELL_DIR/complete/zenteru-complete.zsh"
