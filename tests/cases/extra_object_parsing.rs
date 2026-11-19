//! 관계형 파서 검사. 합성 XML과 바이너리 레코드에서 값, 슬롯 및 개체 소유권을 확인한다.

use std::io::{Cursor, Write};

use rhwp::model::{
    control::{Control, FormObject, FormType},
    document::Document,
    paragraph::Paragraph,
    shape::ShapeObject,
};
use rhwp::parser::{
    control::parse_control,
    hwpx::{parse_hwpx, section::parse_hwpx_section},
    record::Record,
    tags,
};
use rhwp::serializer::{control::serialize_control, hwpx::serialize_hwpx};

/// `runs`를 담은 한 문단을 구역 XML 파서로 읽어 반환한다.
fn paragraph(runs: &str) -> Paragraph {
    parse_hwpx_section(&format!(
        r#"<s:sec xmlns:s="http://www.hancom.co.kr/hwpml/2011/section"
               xmlns:p="http://www.hancom.co.kr/hwpml/2011/paragraph"
               xmlns:c="http://www.hancom.co.kr/hwpml/2011/core">
            <p:p paraPrIDRef="0">{runs}</p:p>
        </s:sec>"#
    ))
    .unwrap()
    .paragraphs
    .remove(0)
}

/// `runs`에 동영상과 썸네일 참조를 연결하고 삭제 변경 ID 9를 숨긴 문서를 반환한다.
fn package(runs: &str) -> Document {
    let section = format!(
        r#"<s:sec xmlns:s="http://www.hancom.co.kr/hwpml/2011/section"
                  xmlns:p="http://www.hancom.co.kr/hwpml/2011/paragraph">
             <p:p paraPrIDRef="0">{runs}</p:p></s:sec>"#
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in [
        ("mimetype", "application/hwp+zip"),
        (
            "Contents/content.hpf",
            r#"<opf:package xmlns:opf="http://www.idpf.org/2007/opf/">
                <opf:manifest>
                  <opf:item id="s" href="Contents/section0.xml" media-type="application/xml"/>
                  <opf:item id="preview" href="BinData/pre.png" media-type="image/png"/>
                  <opf:item id="clip" href="Media/clip.mp4" media-type="video/mp4"/>
                </opf:manifest>
                <opf:spine><opf:itemref idref="s"/></opf:spine></opf:package>"#,
        ),
        (
            "Contents/header.xml",
            r#"<hh:head xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head" secCnt="1">
                <hh:refList>
                  <hh:charProperties itemCnt="1"><hh:charPr id="0" height="1000"></hh:charPr></hh:charProperties>
                  <hh:paraProperties itemCnt="1"><hh:paraPr id="0"></hh:paraPr></hh:paraProperties>
                </hh:refList>
                <hh:trackChanges itemCnt="1"><hh:trackChange id="9" type="Delete" hide="1"/>
                </hh:trackChanges></hh:head>"#,
        ),
        ("Contents/section0.xml", &section),
        ("Media/clip.mp4", "동영상 바이트"),
        ("BinData/pre.png", "썸네일 바이트"),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    parse_hwpx(&zip.finish().unwrap().into_inner()).unwrap()
}

/// chval의 본문, 엔티티와 CDATA를 인접 텍스트와 순서대로 복원하는지 검사한다.
#[test]
fn chval_keeps_character_value() {
    let p = paragraph(
        r#"<p:run><p:t>앞<p:chval>가&amp;<![CDATA[<나>]]></p:chval><p:chval/>뒤</p:t></p:run>"#,
    );
    assert_eq!(p.text, "앞가&<나>뒤");
    assert_eq!(p.char_offsets, [0, 1, 2, 3, 4, 5, 6]);
}

/// unknownch의 원시 값과 표시 위치를 개체 슬롯, 변경 추적 및 저장 왕복과 함께 검사한다.
#[test]
fn unknownch_keeps_utf16_unit_and_position() {
    let document = package(
        r#"<p:run charPrIDRef="0"><p:t>앞😀</p:t><p:textart text="그림"/>
        <p:t>중<p:unknownch unknownchar="55296"/><p:unknownch unknownchar="65"></p:unknownch>뒤
        <p:deleteBegin Id="1" TcId="9"/><p:unknownch unknownchar="56320"/><p:deleteEnd Id="1" TcId="9"/></p:t></p:run>"#,
    );
    let saved = parse_hwpx(&serialize_hwpx(&document).unwrap()).unwrap();
    for doc in [&document, &saved] {
        let p = doc.sections[0]
            .paragraphs
            .iter()
            .find(|p| !p.unknown_chars.is_empty())
            .unwrap();
        assert!(p.text.starts_with("앞😀중\u{FFFD}A뒤"));
        let marks = &p.unknown_chars;
        assert_eq!(marks.len(), 2);
        assert_eq!((marks[0].char_idx, marks[0].code_unit), (3, 0xd800));
        assert_eq!((marks[1].char_idx, marks[1].code_unit), (4, 65));
        assert_eq!(p.char_offsets[marks[0].char_idx], p.char_offsets[2] + 1);
        let mut edited = p.clone();
        edited.insert_text_at(1, "한");
        assert_eq!(edited.unknown_chars[0].char_idx, 4);
        edited.delete_text_at(4, 1);
        assert_eq!(edited.unknown_chars.len(), 1);
        assert_eq!(
            (
                edited.unknown_chars[0].char_idx,
                edited.unknown_chars[0].code_unit
            ),
            (4, 65)
        );
    }
}

/// `p`에서 지정한 `index`의 폼을 반환한다. 다른 컨트롤이면 검사에 실패한다.
fn form(p: &Paragraph, index: usize) -> &FormObject {
    let Control::Form(form) = &p.controls[index] else {
        panic!("양식 컨트롤")
    };
    form
}

/// 목록 상자의 값·표시 이름, 선택값, 배치와 문단 슬롯을 검사한다.
#[test]
fn list_box_keeps_selection_and_items() {
    let p = paragraph(
        r#"<p:run><p:t>앞</p:t><p:listBox selectedValue="B" topIdx="1" itemHeight="-1">
          <p:listItem value="A" displayText="가"/>
          <p:listItem value="B" displayText="나&amp;다"></p:listItem>
          <p:sz width="700" height="600"/><p:pos treatAsChar="1"/>
        </p:listBox><p:t>뒤</p:t></p:run>"#,
    );
    let list = form(&p, 0);
    assert_eq!(list.form_type, FormType::ListBox);
    assert_eq!(list.text, "B");
    assert_eq!(list.properties["listItem0"], "A");
    assert_eq!(list.properties["listItem1"], "B");
    assert_eq!(list.properties["listItemDisplay1"], "나&다");
    assert_eq!(list.properties["TopIdx"], "1");
    assert_eq!(list.properties["ItemHeight"], "-1");
    assert_eq!(list.common.width, 700);
    assert!(list.common.treat_as_char);
    assert_eq!(p.control_text_positions(), [1]);
}

/// 자기닫힘 스크롤 막대에서 음수 값, 범위, 방향, 뒤 형제의 보존을 검사한다.
#[test]
fn scroll_bar_keeps_range_value_and_direction() {
    let p = paragraph(
        r#"<p:run><p:t>앞</p:t><p:scrollBar name="조절" min="-9" max="20" value="-2"
             type="VERTICAL" page="3" smallChange="1" largeChange="5" delay="30" enabled="0"/>
             <p:t>뒤</p:t></p:run>"#,
    );
    let bar = form(&p, 0);
    assert_eq!(bar.form_type, FormType::ScrollBar);
    assert_eq!(bar.name, "조절");
    assert_eq!(bar.value, -2);
    assert!(!bar.enabled);
    for (name, value) in [
        ("min", "-9"),
        ("max", "20"),
        ("type", "VERTICAL"),
        ("page", "3"),
        ("smallChange", "1"),
        ("largeChange", "5"),
        ("delay", "30"),
    ] {
        assert_eq!(bar.properties[name], value);
    }
    assert_eq!(p.control_text_positions(), [1]);
    assert_eq!(p.text, "앞뒤");
}

/// OPF의 의미 있는 ID를 동영상·썸네일에 연결하고 저장 뒤 리소스 바이트도 검사한다.
#[test]
fn video_links_both_manifest_resources() {
    let document = package(
        r#"<p:run charPrIDRef="0"><p:t>앞</p:t>
           <p:video videotype="Local" fileIDRef="clip" imageIDRef="preview" tag="a&amp;b">
             <p:sz width="32000" height="18000"/><p:pos treatAsChar="1"/>
           </p:video><p:t>뒤</p:t>
           <p:deleteBegin Id="1" TcId="9"/><p:video videotype="Web" tag="숨김"/><p:deleteEnd Id="1" TcId="9"/>
        </p:run>"#,
    );
    let saved = parse_hwpx(&serialize_hwpx(&document).unwrap()).unwrap();
    for doc in [&document, &saved] {
        let p = doc.sections[0]
            .paragraphs
            .iter()
            .find(|p| p.text == "앞뒤")
            .unwrap();
        let videos: Vec<_> = p
            .controls
            .iter()
            .zip(p.control_text_positions())
            .filter_map(|(control, position)| match control {
                Control::Shape(shape) => match shape.as_ref() {
                    ShapeObject::Video(video) => Some((video, position)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        let [(video, 1)] = videos.as_slice() else {
            panic!("문단의 동영상 슬롯")
        };
        assert_eq!(video.video_type, "Local");
        assert_eq!(video.tag, "a&b");
        assert_eq!(video.common.width, 32000);
        for (id, contents) in [
            (video.file_bin_data_id, "동영상 바이트"),
            (video.image_bin_data_id, "썸네일 바이트"),
        ] {
            let asset = doc
                .bin_data_content
                .iter()
                .find(|item| Some(item.id) == id)
                .unwrap();
            assert_eq!(asset.data.load(), contents.as_bytes());
        }
    }
}

/// 그룹 자식의 ctrlID, 꼭짓점과 배치를 해당 그룹에 보존하는지 검사한다.
#[test]
fn unknown_shape_keeps_identifier_corners_and_parent() {
    let p = paragraph(
        r#"<p:run><p:t>앞</p:t><p:container><p:unknownObj ctrlID="$x&amp;z">
          <c:pt0 x="-10" y="20"/><c:pt1 x="60" y="20"/>
          <c:Point2 x="60" y="80"/><c:pt3 x="-10" y="80"/>
          <p:sz width="70" height="60"/><p:offset x="13" y="17"/>
          </p:unknownObj></p:container><p:t>뒤</p:t></p:run>"#,
    );
    let [Control::Shape(shape)] = p.controls.as_slice() else {
        panic!("묶음 슬롯 하나")
    };
    let ShapeObject::Group(group) = shape.as_ref() else {
        panic!("묶음 도형")
    };
    let [ShapeObject::Unknown(obj)] = group.children.as_slice() else {
        panic!("미확인 도형")
    };
    assert_eq!(p.text, "앞뒤");
    assert_eq!(p.control_text_positions(), [1]);
    assert_eq!(obj.ctrl_id, "$x&z");
    assert_eq!(
        obj.corners.map(|p| (p.x, p.y)),
        [(-10, 20), (60, 20), (60, 80), (-10, 80)]
    );
    assert_eq!(obj.common.width, 70);
    assert_eq!(
        (
            obj.drawing.shape_attr.offset_x,
            obj.drawing.shape_attr.offset_y
        ),
        (13, 17)
    );
}

/// HWPX 저장 왕복에서 폼 종류와 알 수 없는 도형의 식별자를 검사한다.
#[test]
fn hwpx_roundtrip_keeps_extra_form_and_shape_types() {
    let original = package(
        r#"<p:run charPrIDRef="0"><p:t>검사</p:t>
          <p:listBox selectedValue="B"><p:listItem value="B" displayText="나"/></p:listBox>
          <p:scrollBar min="-7" max="321" value="50" type="HORIZONTAL"/>
          <p:unknownObj ctrlID="$unk"><p:sz width="10" height="20"/></p:unknownObj>
        </p:run>"#,
    );
    let saved = parse_hwpx(&serialize_hwpx(&original).unwrap()).unwrap();
    let p = saved.sections[0]
        .paragraphs
        .iter()
        .find(|p| p.text == "검사")
        .unwrap();
    let controls: Vec<_> = p
        .controls
        .iter()
        .filter(|control| matches!(control, Control::Form(_) | Control::Shape(_)))
        .collect();
    let [Control::Form(list), Control::Form(bar), Control::Shape(shape)] = controls.as_slice()
    else {
        panic!("저장 후 세 개체의 종류와 순서")
    };
    assert_eq!(
        (list.form_type, list.text.as_str()),
        (FormType::ListBox, "B")
    );
    assert_eq!(list.properties["listItemDisplay0"], "나");
    assert_eq!((bar.form_type, bar.value), (FormType::ScrollBar, 50));
    assert_eq!(bar.properties["type"], "HORIZONTAL");
    assert!(matches!(shape.as_ref(), ShapeObject::Unknown(obj) if obj.ctrl_id == "$unk"));
}

/// 타입 `tag`와 `level`로 합성 레코드를 구성해 반환한다.
fn record(tag: u16, level: u16, data: Vec<u8>) -> Record {
    Record {
        tag_id: tag,
        level,
        size: data.len() as u32,
        data,
    }
}

/// `id`와 `level`을 가진 도형 요소 레코드를 반환한다.
fn component(id: u32, level: u16) -> Record {
    record(
        tags::HWPTAG_SHAPE_COMPONENT,
        level,
        id.to_le_bytes().to_vec(),
    )
}

/// 합성 `records`를 GSO로 파싱하여 도형을 반환한다.
fn shape(records: &[Record]) -> Box<ShapeObject> {
    let Control::Shape(shape) = parse_control(tags::CTRL_GEN_SHAPE, &[], records) else {
        panic!("GSO 도형")
    };
    shape
}

/// 그룹의 OLE 자식이 잘린 레코드에서 살아남고 정상 레코드의 리소스 ID도 읽는지 검사한다.
#[test]
fn hwp_group_ole_keeps_own_payload_and_handles_truncation() {
    let full: Vec<_> = [1u32, 7200, 3600, 3]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    for (bytes, id) in [(full.as_slice(), 3), (&full[..9], 0)] {
        let root = shape(&[
            component(tags::SHAPE_CONTAINER_ID, 1),
            component(tags::SHAPE_OLE_ID, 2),
            record(tags::HWPTAG_SHAPE_COMPONENT_OLE, 3, bytes.to_vec()),
        ]);
        let ShapeObject::Group(group) = root.as_ref() else {
            panic!("묶음 도형")
        };
        let [ShapeObject::Ole(ole)] = group.children.as_slice() else {
            panic!("그룹 OLE")
        };
        assert_eq!(ole.bin_data_id, id);
        assert_eq!(ole.raw_tag_data, bytes);
    }
}

/// 직접 차트와 자손 차트의 페이로드 소유권을 구분하고 HWP 왕복까지 검사한다.
#[test]
fn hwp_chart_ownership_survives_groups_and_roundtrip() {
    let original = shape(&[
        component(tags::SHAPE_CONTAINER_ID, 1),
        component(tags::SHAPE_RECT_ID, 2),
        record(tags::HWPTAG_SHAPE_COMPONENT_RECTANGLE, 3, vec![0; 33]),
        record(tags::HWPTAG_CHART_DATA, 4, vec![9]),
        component(tags::SHAPE_CONTAINER_ID, 2),
        record(tags::HWPTAG_SHAPE_COMPONENT_CONTAINER, 3, vec![]),
        component(tags::SHAPE_OLE_ID, 3),
        record(tags::HWPTAG_CHART_DATA, 4, vec![1, 2]),
    ]);
    let mut records = Vec::new();
    serialize_control(&Control::Shape(original.clone()), 0, None, &mut records);
    for result in [original, shape(&records[1..])] {
        let ShapeObject::Group(group) = result.as_ref() else {
            panic!("차트의 바깥 묶음")
        };
        let [ShapeObject::Rectangle(_), ShapeObject::Group(nested)] = group.children.as_slice()
        else {
            panic!("그룹 앞 도형과 중첩 그룹")
        };
        let [ShapeObject::Chart(chart)] = nested.children.as_slice() else {
            panic!("중첩 차트")
        };
        assert_eq!(chart.raw_chart_data, [1, 2]);
    }
    let top = shape(&[
        component(tags::SHAPE_OLE_ID, 1),
        record(tags::HWPTAG_CHART_DATA, 2, vec![3]),
    ]);
    assert!(matches!(top.as_ref(), ShapeObject::Chart(chart) if chart.raw_chart_data == [3]));
}
