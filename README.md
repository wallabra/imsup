# impsup

Scrapes printers in the network, and generates a report of scraped data, in
either JSON or CSV format. (CSV support incomplete.)

Currently supports only some HP printers. Supported models list underway.

## Usage

Run the program.

See `--help` for argument help:

```
Scans for HP printers, from a list of IPs and from the network, and scrapes their internal webpages for information, producing a JSON report that can be written to stdout or to a file

Usage: impsupri [OPTIONS]

Options:
  -d, --disable-ip-list
          Disables reading from an IP list file

  -i, --ip-list <IP_LIST>
          File from which to read a list of IPs to always contact

          These IPs are processed independently from the scanner's, but no same IP is contacted twice.

          Defaults to "impressoras.txt" for historical reasons.

  -s, --scan-mode <SCAN_MODE>
          Sets the network scanning mode.

          Defaults to 'cluster'.

          Possible values:
          - none:    Skips scanning for printers in the network
          - network: Scans the current network for printers
          - cluster: Scans the current network, and all neighboring networks, for printers

  -w, --writeless
          Disables writing the JSON to stdout

  -n, --no-out
          Disables writing the JSON to a file

  -o, --out <OUT>
          Output filename of JSON report.

          Defaults to "relatorio_impressoras.<ext>" for historical reasons.

  -q, --quiet <QUIET>
          Disable non-logger status prints

  -t, --contact-parallel <CONTACT_PARALLEL>
          Number of simultaneous concurrent printer scraper tasks to run.

          The higher, the faster the scanner printers list will be processed, but the higher the chance of a contact failing due to overload.

          The default is 4.

  -p, --ping-parallel <PING_PARALLEL>
          Number of simultaneous ping tasks to run.

          The default is 512.

  -m, --output-mode <OUTPUT_MODE>
          Sets the kind of report to generate.

          Defaults to 'printed_pages' for historical reasons.

          Possible values:
          - none:           No output
          - json:           JSON output
          - printed_values: The # of pages printed per color cartridge per printer. Produces a CSV with 5 columns: serial  c  m  y  k

  -h, --help
          Print help (see a summary with '-h')
```
