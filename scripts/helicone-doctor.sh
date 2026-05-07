#!/usr/bin/env bash
# Check the local Helicone stack that Calciforge provisions for gateway
# observability. This intentionally checks the browser-visible dashboard path,
# not only the gateway write path.

set -euo pipefail

CONTAINER="${CALCIFORGE_HELICONE_CONTAINER:-calciforge-helicone}"
EMAIL="${CALCIFORGE_HELICONE_DASHBOARD_USER_EMAIL:-${1:-}}"
DB="${CALCIFORGE_HELICONE_DB:-helicone_test}"

failures=0

ok() { printf 'ok: %s\n' "$*"; }
warn() { printf 'warn: %s\n' "$*" >&2; }
fail() { printf 'error: %s\n' "$*" >&2; failures=$((failures + 1)); }

require_container() {
    if ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
        fail "container '$CONTAINER' is not running"
        exit 1
    fi
    ok "container '$CONTAINER' is running"
}

container_env() {
    docker exec "$CONTAINER" sh -lc "printf '%s\n' \"\${$1:-}\""
}

check_env_and_ports() {
    local app_url jawn_url s3_url
    app_url="$(container_env NEXT_PUBLIC_APP_URL)"
    jawn_url="$(container_env NEXT_PUBLIC_HELICONE_JAWN_SERVICE)"
    s3_url="$(container_env S3_ENDPOINT)"

    [[ -n "$app_url" ]] && ok "dashboard URL: $app_url" || fail "NEXT_PUBLIC_APP_URL is empty"
    [[ -n "$jawn_url" ]] && ok "browser Jawn URL: $jawn_url" || fail "NEXT_PUBLIC_HELICONE_JAWN_SERVICE is empty"
    [[ -n "$s3_url" ]] && ok "S3 endpoint: $s3_url" || warn "S3_ENDPOINT is empty"

    if [[ "$app_url" =~ ^http://([^/:]+):([0-9]+) ]]; then
        local app_host="${BASH_REMATCH[1]}"
        if [[ "$app_host" != "127.0.0.1" && "$app_host" != "localhost" && "$jawn_url" =~ ^http://127\.0\.0\.1: ]]; then
            fail "LAN dashboard advertises loopback Jawn URL; remote browsers will show no data"
        fi
    fi

    docker port "$CONTAINER" 3000/tcp >/dev/null 2>&1 && ok "dashboard port is published" || fail "dashboard port 3000 is not published"
    docker port "$CONTAINER" 8585/tcp >/dev/null 2>&1 && ok "Jawn port is published" || fail "Jawn port 8585 is not published"
}

psql_query() {
    docker exec "$CONTAINER" psql "postgresql://postgres:password@localhost:5432/$DB" -Atc "$1"
}

clickhouse_query() {
    docker exec "$CONTAINER" clickhouse-client --query "$1"
}

check_user_and_key() {
    local email_filter=""
    if [[ -n "$EMAIL" ]]; then
        email_filter="where u.email = '$EMAIL'"
    fi

    local user_rows
    user_rows="$(psql_query "select count(*) from public.\"user\" u $email_filter;")" || {
        fail "could not query Helicone users"
        return
    }
    [[ "$user_rows" -gt 0 ]] && ok "dashboard user exists (${EMAIL:-any email})" || fail "dashboard user not found: ${EMAIL:-no email supplied}"

    if [[ -n "$EMAIL" ]]; then
        local credential_rows owner_rows
        credential_rows="$(psql_query "select count(*) from public.\"user\" u join public.account a on a.\"userId\" = u.id where u.email = '$EMAIL' and a.\"providerId\" = 'credential' and length(coalesce(a.password, '')) > 0;")"
        owner_rows="$(psql_query "select count(*) from public.\"user\" u join public.organization_member om on om.member = u.auth_user_id or om.\"user\" = u.id where u.email = '$EMAIL' and om.org_role in ('owner', 'admin');")"
        [[ "$credential_rows" -gt 0 ]] && ok "dashboard credential password is present" || fail "dashboard credential password is missing for $EMAIL"
        [[ "$owner_rows" -gt 0 ]] && ok "dashboard user has owner/admin org membership" || fail "dashboard user lacks owner/admin org membership"
    fi

    local rw_keys
    rw_keys="$(psql_query "select count(*) from public.helicone_api_keys where api_key_name = 'Calciforge local gateway' and key_permissions like '%r%' and key_permissions like '%w%' and soft_delete = false;")"
    [[ "$rw_keys" -gt 0 ]] && ok "Calciforge local gateway API key has read/write permissions" || fail "Calciforge local gateway API key is missing read/write permissions"

    if [[ -n "$EMAIL" ]]; then
        local visible_key_rows
        visible_key_rows="$(psql_query "select count(*) from public.helicone_api_keys k join public.\"user\" u on lower(u.email) = lower('$EMAIL') join public.organization_member om on (om.member = u.auth_user_id or om.\"user\" = u.id) and om.organization = k.organization_id where k.api_key_name = 'Calciforge local gateway' and k.key_permissions like '%r%' and k.key_permissions like '%w%' and k.soft_delete = false;")"
        [[ "$visible_key_rows" -gt 0 ]] \
            && ok "Calciforge local gateway API key belongs to a $EMAIL organization" \
            || fail "Calciforge local gateway API key is not attached to a $EMAIL organization; dashboard may show no traffic"
    fi
}

check_request_visibility() {
    clickhouse_query "exists default.request_response_rmt" | grep -qx 1 \
        && ok "ClickHouse request_response_rmt table exists" \
        || { fail "ClickHouse request_response_rmt table is missing"; return; }

    local rows
    rows="$(clickhouse_query "select count() from default.request_response_rmt")" || {
        fail "could not query ClickHouse request_response_rmt"
        return
    }
    [[ "$rows" -gt 0 ]] && ok "ClickHouse has $rows request row(s)" || warn "ClickHouse has no request rows yet"

    if [[ -n "$EMAIL" ]]; then
        local orgs org_filter org_rows
        orgs="$(psql_query "select string_agg(quote_literal(om.organization::text), ',') from public.\"user\" u join public.organization_member om on om.member = u.auth_user_id or om.\"user\" = u.id where u.email = '$EMAIL';")"
        if [[ -n "$orgs" ]]; then
            org_filter="$(printf '%s' "$orgs" | tr -d "'")"
            org_rows="$(clickhouse_query "select count() from default.request_response_rmt where organization_id in (${orgs})")" || org_rows=0
            if [[ "$org_rows" -gt 0 ]]; then
                ok "ClickHouse has $org_rows request row(s) visible to $EMAIL org(s): $org_filter"
            elif [[ "$rows" -gt 0 ]]; then
                fail "ClickHouse has request rows, but none are visible to $EMAIL org(s): $org_filter"
            else
                warn "ClickHouse has no rows for $EMAIL org(s): $org_filter"
            fi
        fi
    fi
}

require_container
check_env_and_ports
check_user_and_key
check_request_visibility

if [[ "$failures" -gt 0 ]]; then
    printf '\nHelicone doctor failed with %s error(s).\n' "$failures" >&2
    exit 1
fi

printf '\nHelicone doctor passed.\n'
