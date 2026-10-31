# zenteru shell integration for bash (3.2+). See zenteru.zsh for what it does.
[ -n "${_ZENTERU_LOADED-}" ] && return
_ZENTERU_LOADED=1
_zt_at_prompt=""
_zt_running=""
_zt_applied=""
_zt_saved_file=""

_zt_osc() { printf '\e]%s\a' "$1" >/dev/tty 2>/dev/null; }

_zenteru_env_apply() {
  local d="${ZENTERU_SESSION_DIR-}" n
  _zt_applied=""
  [ -z "$d" ] && return
  if [ -e "$d/skip-env-once" ]; then rm -f -- "$d/skip-env-once"; return; fi
  [ -r "$d/env.names" ] && [ -r "$d/env" ] || return
  _zt_saved_file="$d/env.saved"
  : > "$_zt_saved_file"; chmod 600 "$_zt_saved_file"
  while IFS= read -r n; do
    [ -z "$n" ] && continue
    if [ -n "${!n+x}" ]; then printf 'export %s=%q\n' "$n" "${!n}" >> "$_zt_saved_file"; fi
    _zt_applied="$_zt_applied $n"
  done < "$d/env.names"
  . "$d/env"
}

_zenteru_env_restore() {
  local n
  for n in $_zt_applied; do unset "$n"; done
  if [ -n "$_zt_saved_file" ] && [ -r "$_zt_saved_file" ]; then . "$_zt_saved_file"; rm -f -- "$_zt_saved_file"; fi
  _zt_applied=""
  _zt_saved_file=""
}

_zenteru_preexec() {
  [ -z "$_zt_at_prompt" ] && return
  [ -n "${COMP_LINE-}" ] && return
  _zt_at_prompt=""
  local cmd
  cmd=$(HISTTIMEFORMAT= builtin history 1 | sed 's/^ *[0-9]* *//')
  cmd="${cmd//\\/\\\\}"; cmd="${cmd//;/\\x3b}"
  _zt_osc "633;E;$cmd"
  _zt_osc "133;C"
  _zt_running=1
  _zenteru_env_apply
}

_zenteru_precmd() {
  local ret="${_zt_ret:-$?}"
  if [ -n "$_zt_running" ]; then _zt_osc "133;D;$ret"; _zt_running=""; fi
  _zenteru_env_restore
  _zt_osc "7;file://${HOSTNAME}${PWD// /%20}"
  _zt_osc "133;A"
  _zt_at_prompt=1
}

# @info / @version / @zenteru (bash 4+ can bind shell functions to keys).
_zenteru_info() { [ -r "${ZENTERU_SESSION_DIR-}/info.ans" ] && cat -- "$ZENTERU_SESSION_DIR/info.ans"; }
if [ "${BASH_VERSINFO[0]:-0}" -ge 4 ]; then bind -x '"\e[9236~": _zenteru_info' 2>/dev/null; fi

trap '_zenteru_preexec' DEBUG
PROMPT_COMMAND="_zt_ret=\$?;${PROMPT_COMMAND:+$PROMPT_COMMAND;}_zenteru_precmd"
case "$PS1" in *'133;B'*) ;; *) PS1="$PS1"'\[\e]133;B\a\]' ;; esac

[ -r "$ZENTERU_SHELL_DIR/complete/zenteru-complete.bash" ] && . "$ZENTERU_SHELL_DIR/complete/zenteru-complete.bash"
