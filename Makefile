#
# We differentiate between release / debug build types using the BUILD_TYPE
# environment variable.
#
BUILD_TYPE ?= debug
ifeq ($(BUILD_TYPE),release)
	PG_CONFIGURE_OPTS = --enable-debug
	PG_CFLAGS = -O2 -g3 $(CFLAGS)
	CARGO_BUILD_FLAGS += --release
else ifeq ($(BUILD_TYPE),debug)
	PG_CONFIGURE_OPTS = --enable-debug --enable-cassert --enable-depend
	PG_CFLAGS = -O0 -g3 $(CFLAGS)
else
	$(error Bad build type `$(BUILD_TYPE)', see Makefile for options)
endif

.PHONY: all
all: xactserver postgres


#
# Transaction server
#

.PHONY: xactserver
xactserver:
	+@echo "Compiling xactserver"
	cargo build $(CARGO_BUILD_FLAGS)


#
# PostgreSQL
#

# Configure PostgreSQL
tmp_install/build/config.status:
	+@echo "Configuring postgres build"
	mkdir -p tmp_install/build
	(cd tmp_install/build && \
	../../postgres/configure CFLAGS='$(PG_CFLAGS)' \
		$(PG_CONFIGURE_OPTS) \
		--prefix=$(abspath tmp_install))

# Nicer alias for running 'configure'
.PHONY: postgres-configure
postgres-configure: tmp_install/build/config.status

# Compile and install PostgreSQL and contrib/remotexact
.PHONY: postgres
postgres: postgres-configure
	+@echo "Compiling PostgreSQL"
	$(MAKE) -C tmp_install/build MAKELEVEL=0 install
	+@echo "Compiling contrib/remotexact"
	$(MAKE) -C tmp_install/build/contrib/remotexact install


#
# Cleaning up
#
.PHONY: postgres-clean
postgres-clean:
	$(MAKE) -C tmp_install/build MAKELEVEL=0 clean

# This doesn't remove the effects of 'configure'.
.PHONY: clean
clean:
	cd tmp_install/build && $(MAKE) clean
	cargo clean

# This removes everything
.PHONY: distclean
distclean:
	rm -rf tmp_install
	cargo clean
