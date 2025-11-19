
#include <stdint.h>
#include <Rinternals.h>
#include <R_ext/Parse.h>

#include "rust/api.h"

static uintptr_t TAGGED_POINTER_MASK = (uintptr_t)1;

SEXP handle_result(SEXP res_) {
    uintptr_t res = (uintptr_t)res_;

    // An error is indicated by tag.
    if ((res & TAGGED_POINTER_MASK) == 1) {
        // Remove tag
        SEXP res_aligned = (SEXP)(res & ~TAGGED_POINTER_MASK);

        // Currently, there are two types of error cases:
        //
        //   1. Error from Rust code
        //   2. Error from R's C API, which is caught by R_UnwindProtect()
        //
        if (TYPEOF(res_aligned) == CHARSXP) {
            // In case 1, the result is an error message that can be passed to
            // Rf_errorcall() directly.
            Rf_errorcall(R_NilValue, "%s", CHAR(res_aligned));
        } else {
            // In case 2, the result is the token to restart the
            // cleanup process on R's side.
            R_ContinueUnwind(res_aligned);
        }
    }

    return (SEXP)res;
}

SEXP savvy_run_od2net_counts__impl(
    SEXP c_arg__osm_pbf_path,
    SEXP c_arg__origin_lon,
    SEXP c_arg__origin_lat,
    SEXP c_arg__dest_lon,
    SEXP c_arg__dest_lat) {
    SEXP res = savvy_run_od2net_counts__ffi(
        c_arg__osm_pbf_path,
        c_arg__origin_lon,
        c_arg__origin_lat,
        c_arg__dest_lon,
        c_arg__dest_lat);
    return handle_result(res);
}


static const R_CallMethodDef CallEntries[] = {
    {"savvy_run_od2net_counts__impl", (DL_FUNC) &savvy_run_od2net_counts__impl, 5},
    {NULL, NULL, 0}
};

void R_init_od2net(DllInfo *dll) {
    R_registerRoutines(dll, NULL, CallEntries, NULL, NULL);
    R_useDynamicSymbols(dll, FALSE);

    // Functions for initialzation, if any.

}
