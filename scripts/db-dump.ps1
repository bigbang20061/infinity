# Dump every table (and every sequence no table owns) of the running database to
# db/<schema>/<name>.sql.gz, the per-table dumps scripts/db-init.sh loads into a fresh
# database on first `docker compose up`. Replaces the schema folders under db/.
param([string]$Container = "infinity-db-1")
Set-Location (Join-Path $PSScriptRoot "..")

$dump = @'
set -eu
rm -rf /tmp/dbdump && mkdir -p /tmp/dbdump
psql -U infinity -d infinity -At -F " " -c "
    SELECT schemaname, tablename FROM pg_tables WHERE schemaname NOT IN ('pg_catalog', 'information_schema')
    UNION ALL
    SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relkind = 'S' AND n.nspname NOT IN ('pg_catalog', 'information_schema')
      AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid AND d.deptype IN ('a', 'i'))
    ORDER BY 1, 2" |
while read schema name; do
    mkdir -p "/tmp/dbdump/$schema"
    pg_dump -U infinity -d infinity --no-owner -t "\"$schema\".\"$name\"" | gzip -6 > "/tmp/dbdump/$schema/$name.sql.gz"
done
find /tmp/dbdump -name "*.sql.gz" | wc -l
'@
# Copied in as a file: piping it would turn its line ends into CRLF.
$tmp = New-TemporaryFile
[IO.File]::WriteAllText($tmp, ($dump -replace "`r", ""))
docker cp $tmp "${Container}:/tmp/db-dump.sh" | Out-Null
Remove-Item $tmp
$count = docker exec $Container sh /tmp/db-dump.sh
if ($LASTEXITCODE -ne 0) { throw "dump failed" }
docker exec $Container rm -f /tmp/db-dump.sh

Get-ChildItem db -Directory | Remove-Item -Recurse -Force
docker cp "${Container}:/tmp/dbdump/." db/
docker exec $Container rm -rf /tmp/dbdump
"$count dumps written under db/"
