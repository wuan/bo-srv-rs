# Service Setup (Ubuntu, systemd)

How to install and run `bo-webservice` as a systemd service on Ubuntu, with
rsyslog routing the service's log output into `/var/log/blitzortung/`.

For the database schema and maintenance see
[database_setup.md](database_setup.md); for the configuration file layout and
all settings see the [Run](../README.md#run) section of the README.

## Install the binaries

The main source for the binaries is the release package of the
[bo-srv-rs GitHub repository](https://github.com/wuan/bo-srv-rs/releases).
Each release ships a `bo-srv-rs-<version>.zip` containing all seven binaries
(`bo-webservice`, `bo-db`, `bo-import`, `bo-import-websocket`, `bo-update`,
`bo-cluster`, `bo-servicelog-stats`):

```sh
VERSION=v0.1.22
curl -fL -o /tmp/bo-srv-rs-${VERSION}.zip \
  https://github.com/wuan/bo-srv-rs/releases/download/${VERSION}/bo-srv-rs-${VERSION}.zip
sudo unzip -o /tmp/bo-srv-rs-${VERSION}.zip -d /usr/local/bin
sudo chmod 755 /usr/local/bin/bo-*
```

Alternatively, build from source and copy the binaries to `/usr/local/bin`
(`install_release.sh` does exactly this):

```sh
cargo build --release
sudo ./install_release.sh
```

## Configuration

The service reads `/etc/blitzortung.conf` (see the README for the full format):

```ini
[webservice]
port = 7081

[db]
host = localhost
port = 5432
dbname = blitzortung
username = blitzortung
password = secret
connection_count = 3

[auth]
username = <blitzortung.org account>
password = <blitzortung.org password>
```

## Log directory

Create the directory that holds the daily usage-log files
(`servicelog_YYYY-MM-DD`) and the rsyslog-written service log:

```sh
sudo mkdir -p /var/log/blitzortung
```

The directory must exist and be writable by the service user. On Ubuntu
rsyslogd runs as the `syslog` user, so grant it access with an ACL:

```sh
sudo setfacl -m u:syslog:rwx /var/log/blitzortung/
```

## systemd unit

Create `/etc/systemd/system/blitzortung-webservice` (systemd treats a unit
file without the `.service` suffix as a service):

```ini
[Unit]
Description=bo-webservice
After=network.target

[Service]
Type=simple
Environment="BO_DB_CONNECTION_COUNT=40"
Restart=always
ExecStart=/usr/local/bin/bo-webservice --servicelog /var/log/blitzortung/
SyslogIdentifier=bo-webservice

[Install]
Alias=bo-webservice
WantedBy=multi-user.target
```

Notes:

- `--servicelog /var/log/blitzortung/` enables the in-process usage logging
  into that directory (CLI beats the `blitzortung.conf` value, see
  [Usage logging](../README.md#usage-logging)).
- `Environment="BO_DB_CONNECTION_COUNT=40"` overrides the INI
  `connection_count` for this deployment (explicit env vars win over the
  config file).
- `SyslogIdentifier=bo-webservice` tags every journal line, which is what the
  rsyslog rule below matches on.
- `Alias=bo-webservice` makes `systemctl enable` create a `bo-webservice`
  alias, so `systemctl status bo-webservice` works as well.
- The unit runs as `root` (no `User=`); add `User=`/`Group=` if you prefer a
  dedicated account — the servicelog directory must then be writable by that
  user instead.

## rsyslog

Route the tagged journal output into its own file via
`/etc/rsyslog.d/30-blitzortung.conf`:

```
if $programname == 'bo-webservice' then /var/log/blitzortung/bo-webservice.log
& stop
```

`& stop` prevents the lines from also landing in `/var/log/syslog`. Reload
rsyslog after creating the file:

```sh
sudo systemctl restart rsyslog
```

## Enable and start

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now blitzortung-webservice
```

`enable` creates the `bo-webservice` alias; `--now` starts the service
immediately. Check with:

```sh
systemctl status bo-webservice
```

## Logs

There are three separate log outputs:

| Output | Where | Content |
| --- | --- | --- |
| journald | `journalctl -u blitzortung-webservice` | access lines / warnings (structured journal entries under systemd) |
| rsyslog | `/var/log/blitzortung/bo-webservice.log` | the same lines, filtered by `$programname` |
| usage log | `/var/log/blitzortung/servicelog_YYYY-MM-DD` | one tab-separated row per successful grid request, written by the service itself |

Useful commands:

```sh
journalctl -u blitzortung-webservice -f            # follow
journalctl -u blitzortung-webservice -p warning    # warnings and worse
journalctl -t bo-webservice                        # by SyslogIdentifier
```

The daily `servicelog_*` files can be summarised with `bo-servicelog-stats`
(see [Statistics from the log files](../README.md#statistics-from-the-log-files)).

`bo-webservice.log` grows without bound unless rotated, e.g.
`/etc/logrotate.d/blitzortung`:

```
/var/log/blitzortung/bo-webservice.log {
    weekly
    rotate 8
    compress
    missingok
    notifempty
}
```

## Scheduled tasks

`/etc/cron.d/blitzortung-server` runs the importers, the cluster producer and
the statistics report:

```cron
#
# Regular cron jobs for the blitzortung-server
#

SHELL=/bin/bash
PATH=/usr/local/sbin:/usr/local/bin:/sbin:/bin:/usr/sbin:/usr/bin

# m h dom mon dow user  command

# Import the latest strikes, then (re)start the live websocket importer in
# the background. Skipped while a manual vacuumdb is running;
# /tmp/.bo-import.last marks the last successful bo-update.
* * * * *    root    ps >/dev/null -C vacuumdb || (/usr/local/bin/bo-update >/dev/null 2>&1 && touch /tmp/.bo-import.last) && (nohup /usr/local/bin/bo-import-websocket >/dev/null 2>&1 &)

# Calculate clusters from the current strikes and store them, offset by 15 s
# so it runs behind the per-minute import.
* * * * *    root    sleep 15; /usr/local/bin/bo-cluster --insert --metrics >/dev/null 2>&1

# Publish the servicelog statistics report.
*/5 * * * *  root    /usr/local/bin/bo-servicelog-stats --format html --top 50 --output /var/www/html/bo-stats.html

# Compress servicelog files older than one day.
30 0 * * *   root    find /var/log/blitzortung -type f -name 'servicelog_*' ! -name '*.xz' -mtime +1 -exec xz {} +
```

Notes:

- The websocket importer is launched once per minute with `nohup ... &`, which
  acts as a keep-alive: an already-running instance holds the
  `/tmp/.bo-import-websocket.lock` flock, so the new process fails to acquire
  the lock within 10 s and exits (see `src/bin/bo-import-websocket.rs`).
  `bo-update` and `bo-import` protect themselves the same way
  (`/tmp/.bo-update.lock`, `/tmp/.bo-import.lock`).
- The `ps -C vacuumdb` guard skips the import while a manual vacuum is
  running.
- The statistics report defaults to **today (UTC)**, so the nightly `xz`
  compression of older files does not affect it. `bo-servicelog-stats` cannot
  read the `.xz` archives — decompress with `xz -d` first to analyse a past
  day (`--date`/`--all`).

