FROM gcr.io/oss-fuzz-base/base-builder-rust:v1@sha256:3428d0a90b1e7efbe2ba947bd1d0c9a70594986b4b6553804547f1fb24cfc92f
COPY . $SRC/pixtuoid
WORKDIR $SRC/pixtuoid
COPY .clusterfuzzlite/build.sh $SRC/
