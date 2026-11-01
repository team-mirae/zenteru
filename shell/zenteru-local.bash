# zenteru: run a script in the current directory by its bare name (bash 4+,
# which added command_not_found_handle; bash 3.2 ignores this). See
# zenteru-local.zsh for the rules. It only runs when no command of that name
# exists, so it can't shadow one; a typo can run a local file of that name.

_zenteru_local_handler() {
  local name=$1 first= interp=
  if [ -n "$name" ] && [ "${name#*/}" = "$name" ] && [ -f "./$name" ]; then
    local -a run=()
    if [ -x "./$name" ]; then
      run=("./$name")
    else
      IFS= read -r first < "./$name" 2>/dev/null
      case $first in
        '#!'*) interp=${first#\#!}; read -r -a run <<< "$interp"; [ ${#run[@]} -gt 0 ] && run+=("./$name") ;;
      esac
      if [ ${#run[@]} -eq 0 ]; then
        case $name in
          *.sh) run=(sh "./$name") ;;
          *.zsh) run=(zsh "./$name") ;;
          *.bash) run=(bash "./$name") ;;
        esac
      fi
    fi
    if [ ${#run[@]} -gt 0 ]; then
      shift
      printf '\e[2mzenteru: running %s\e[22m\n' "${run[*]}" >&2
      "${run[@]}" "$@"
      return
    fi
  fi
  if declare -F _zenteru_cnf_prev >/dev/null; then
    _zenteru_cnf_prev "$@"
    return
  fi
  printf 'bash: %s: command not found\n' "$name" >&2
  return 127
}

_zenteru_local_install() {
  if declare -F command_not_found_handle >/dev/null &&
     ! declare -f command_not_found_handle | grep -q _zenteru_local_handler; then
    eval "$(declare -f command_not_found_handle | sed '1s/^command_not_found_handle/_zenteru_cnf_prev/')"
  fi
  command_not_found_handle() { _zenteru_local_handler "$@"; }
}
# bash loads zenteru after the user's profile, so their handler already exists.
[ "${BASH_VERSINFO[0]:-0}" -ge 4 ] && _zenteru_local_install
