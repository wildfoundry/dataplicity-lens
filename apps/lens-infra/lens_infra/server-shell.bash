# Dataplicity server shell v1. Bash 4+, local data only, no subprocesses per prompt.
case $- in *i*) ;; *) return ;; esac
[[ ${DP_SHELL-} != off && -z ${_DP_SHELL_LOADED-} ]] || return
_DP_SHELL_LOADED=1

_dp_context() {
    _DP_ENV=unknown _DP_VM=vm _DP_ID=unknown _DP_REV=unknown
    local key value
    while IFS='=' read -r key value; do
        # Context is data, never shell source. Reject controls and shell metacharacters.
        [[ $value && $value != *[!a-zA-Z0-9_.-]* ]] || continue
        case $key in
            environment) _DP_ENV=$value ;;
            vm) _DP_VM=$value ;;
            identity) _DP_ID=${value:0:8} ;;
            configuration_revision) _DP_REV=$value ;;
        esac
    done 2>/dev/null < /etc/dataplicity/server-context
    return 0
}

_dp_capture_status() { _DP_LAST_STATUS=$?; return "$_DP_LAST_STATUS"; }

_dp_render_prompt() {
    local reset='' dim='' identity='' emphasis='' failure='' root=''
    local label=${_DP_ENV^^} width=${COLUMNS:-80} limit path_text
    # Literal parameter expansion inserts the path after prompt decoding. It is never eval'd.
    _DP_CWD=${PWD//[[:cntrl:]]/?}
    if [[ $_DP_CWD == "$HOME" || $_DP_CWD == "$HOME/"* ]]; then
        _DP_CWD="~${_DP_CWD#"$HOME"}"
    fi
    [[ $width =~ ^[0-9]{1,4}$ ]] || width=80
    limit=$((width > 70 ? 48 : 24))
    if ((${#_DP_CWD} > limit)); then _DP_CWD="...${_DP_CWD: -limit}"; fi
    if [[ ${DP_SHELL-} != plain && -z ${NO_COLOR+x} ]]; then
        case ${TERM-} in
            xterm*|screen*|tmux*|linux|rxvt*)
                reset='\[\e[0m\]' dim='\[\e[2m\]' identity='\[\e[36m\]' emphasis='\[\e[1;33m\]'
                case $_DP_ENV in prod|production|prod-*|production-*|*-prod|*-production|*-prod-*|*-production-*) emphasis='\[\e[1;31m\]' ;; esac
                ;;
        esac
    fi
    if ((EUID == 0)); then root=' ROOT'; [[ $reset ]] && identity='\[\e[1;31m\]'; fi
    if ((_DP_LAST_STATUS != 0)); then failure=" ${emphasis}exit ${_DP_LAST_STATUS}${reset}"; fi
    if shopt -q promptvars; then path_text='${_DP_CWD}'; else path_text=${_DP_CWD//\\/\\\\}; fi
    PS1="${emphasis}[${label}${root}]${reset} ${identity}\\u@${_DP_VM}:${_DP_ID}${reset} ${dim}${path_text}${reset}${failure}\\n\\\$ "
    if ((width < 70)); then
        PS1="${emphasis}[${label}${root}]${reset} ${identity}\\u@${_DP_VM:0:12}:${_DP_ID:0:4}${reset}${failure}\\n${dim}${path_text}${reset}\\n\\\$ "
    fi
}

_dp_context
_DP_LAST_STATUS=0
# Preserve existing hooks and capture the command status before any of them run.
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(_dp_capture_status "${PROMPT_COMMAND[@]}" _dp_render_prompt)
else
    PROMPT_COMMAND=(_dp_capture_status "${PROMPT_COMMAND-}" _dp_render_prompt)
fi

# Welcome once per interactive terminal session; command-mode SSH/SCP remain silent.
if [[ -t 0 && -t 1 && -z ${DP_SHELL_WELCOME_SHOWN-} ]]; then
    export DP_SHELL_WELCOME_SHOWN=1
    if [[ -x /usr/local/bin/dp-status ]]; then /usr/local/bin/dp-status --welcome; fi
fi
