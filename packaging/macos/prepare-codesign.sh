#!/bin/zsh

# 为 AuraCoder macOS 构建准备固定的自签名代码签名身份。
set -euo pipefail

if [[ "$(/usr/bin/uname -s)" != "Darwin" ]]; then
    print -u2 "macOS 代码签名准备脚本只能在 macOS 执行"
    exit 1
fi

# 解析脚本所在目录，确保仓库和证书路径不依赖调用方当前目录。
readonly script_directory="${0:A:h}"
readonly repo_root="${script_directory:h:h}"
readonly codesign_name="Yunxiang AuraCoder Signing"
readonly codesign_sha1="B1F0EFB5AC17F0E427F591FEDFE8DA7CAA371274"
# 从 GitHub Actions Secret 或本地环境读取 P12 导入密码和临时证书内容。
readonly macos_codesign_p12_base64="${MACOS_CODESIGN_P12_BASE64:-}"
readonly codesign_password="${MACOS_CODESIGN_P12_PASSWORD:-}"
readonly local_codesign_p12="${repo_root}/packaging/macos/certs/YunxiangAuraCoderSigning.p12"
readonly codesign_keychain="${HOME:?缺少 HOME}/Library/Keychains/YunxiangAuraCoderSigning.keychain-db"
readonly certificate_workspace="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/YunxiangAuraCoderSigning.XXXXXX")"
readonly codesign_cert_pem="${certificate_workspace}/YunxiangAuraCoderSigning.crt"
readonly temporary_codesign_p12="${certificate_workspace}/YunxiangAuraCoderSigning.p12"

# 根据 Secret 是否存在选择临时还原证书或仓库内的本地证书。
# 在解码前登记精确临时目录的清理，保证失败分支不遗留 Secret 还原文件。
trap '/bin/rm -rf -- "${certificate_workspace}"' EXIT
if [[ -n "${macos_codesign_p12_base64}" ]]; then
    if ! print -n -- "${macos_codesign_p12_base64}" | /usr/bin/base64 -D > "${temporary_codesign_p12}"; then
        print -u2 "GitHub macOS 签名证书解码失败"
        exit 1
    fi
    /bin/chmod 600 "${temporary_codesign_p12}"
    if [[ ! -s "${temporary_codesign_p12}" ]]; then
        print -u2 "GitHub macOS 签名证书解码结果为空"
        exit 1
    fi
    readonly codesign_p12="${temporary_codesign_p12}"
else
    readonly codesign_p12="${local_codesign_p12}"
fi

# 清理本次身份准备生成的临时证书文件。
cleanup_certificate_workspace() {
    /bin/rm -rf "${certificate_workspace}"
}

trap cleanup_certificate_workspace EXIT

# 确保专用 keychain 已加入当前用户的 keychain 搜索列表。
ensure_keychain_search_list() {
    local existing_keychains=("${(@f)$(
        /usr/bin/security list-keychains -d user \
            | /usr/bin/sed -e 's/^ *"//' -e 's/"$//'
    )}")
    local keychain
    for keychain in "${existing_keychains[@]}"; do
        if [[ "${keychain}" == "${codesign_keychain}" ]]; then
            return 0
        fi
    done
    /usr/bin/security list-keychains -d user -s \
        "${codesign_keychain}" \
        "${existing_keychains[@]}"
}

# 严格判断专用 keychain 中是否存在指定 SHA1 和名称的签名身份。
codesign_identity_available() {
    local identities
    identities="$(/usr/bin/security find-identity -v -p codesigning "${codesign_keychain}")"
    [[ "${identities}" == *"${codesign_sha1} \"${codesign_name}\""* ]]
}

# 创建、导入并信任 AuraCoder 固定签名证书。
prepare_codesign_identity() {
    if [[ ! -f "${codesign_p12}" ]]; then
        print -u2 "macOS 固定签名证书不存在：${codesign_p12}"
        return 1
    fi

    /bin/mkdir -p "${codesign_keychain:h}"
    if [[ -f "${codesign_keychain}" ]] && ! /usr/bin/security unlock-keychain \
        -p "${codesign_password}" \
        "${codesign_keychain}"; then
        /usr/bin/security delete-keychain "${codesign_keychain}"
    fi
    if [[ ! -f "${codesign_keychain}" ]]; then
        /usr/bin/security create-keychain \
            -p "${codesign_password}" \
            "${codesign_keychain}"
    fi
    /usr/bin/security unlock-keychain \
        -p "${codesign_password}" \
        "${codesign_keychain}"
    ensure_keychain_search_list

    if ! codesign_identity_available; then
        /usr/bin/security import "${codesign_p12}" \
            -k "${codesign_keychain}" \
            -P "${codesign_password}" \
            -T /usr/bin/codesign \
            -T /usr/bin/security
        /usr/bin/security set-key-partition-list \
            -S apple-tool:,apple:,codesign: \
            -s \
            -k "${codesign_password}" \
            "${codesign_keychain}"
        /usr/bin/security find-certificate \
            -c "${codesign_name}" \
            -p "${codesign_keychain}" \
            > "${codesign_cert_pem}"
        /usr/bin/security add-trusted-cert \
            -r trustRoot \
            -p codeSign \
            -k "${codesign_keychain}" \
            "${codesign_cert_pem}"
    fi

    /usr/bin/security unlock-keychain \
        -p "${codesign_password}" \
        "${codesign_keychain}"
    if ! codesign_identity_available; then
        print -u2 "macOS 固定签名身份不可用：${codesign_sha1} ${codesign_name}"
        return 1
    fi
}

prepare_codesign_identity
print "固定 macOS 签名身份：${codesign_sha1} ${codesign_name}"
print "专用签名 keychain：${codesign_keychain}"
