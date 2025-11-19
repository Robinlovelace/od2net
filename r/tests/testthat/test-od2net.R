test_that("od2net_counts rejects mismatched lengths", {
    x <- c(0.0, 1.0)
    short <- c(0.0)

    # The R wrapper checks length(origin_lon) == length(origin_lat) AND length(dest_lon) == length(dest_lat)
    # We want to trigger the Rust error "equal length" which checks across all 4 vectors.
    # So we need valid pairs, but mismatched between pairs.

    # This passes R checks: origin has 2, dest has 1.
    # Rust check should fail because all 4 must be same length.
    expect_error(
        od2net_counts("dummy.pbf", x, x, short, short),
        "equal length"
    )
})

test_that("od2net_counts rejects NAs", {
    vals <- c(0.0, NA)
    ok <- c(0.0, 0.0)

    expect_error(
        od2net_counts("dummy.pbf", ok, ok, ok, vals),
        "NA/NaN"
    )
})
