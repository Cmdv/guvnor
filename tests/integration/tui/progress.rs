use guvnor::tui::progress::stage_no;

#[test]
fn pipeline_stage_index_parses() {
    assert_eq!(stage_no("[0/5] baseline check: node --test"), Some(0));
    assert_eq!(stage_no("[3/5] rework 1/1: implementer gets the failing output"), Some(3));
    assert_eq!(stage_no("no bracket"), None);
}
