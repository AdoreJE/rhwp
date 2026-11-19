//! 관계형 파서 검사. 합성 레코드와 XML에서 개체 소속, 순서, 문자열 보존을 확인한다.

use std::io::{Cursor, Write};

use rhwp::model::{control::Control, shape::ShapeObject};
use rhwp::parser::{control::parse_control, hwpx::parse_hwpx, record::Record, tags};
use rhwp::serializer::{control::serialize_control, hwpx::serialize_hwpx};

/// 추적 변경 ID 7을 숨기는 최소 HWPX 패키지를 만든다.
///
/// `runs`는 단일 문단의 run 내용이다. 완성한 ZIP 바이트를 반환한다.
fn hwpx_with_hidden_deletion(runs: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let section = format!(
        r#"<hs:sec xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section"
            xmlns:p="http://www.hancom.co.kr/hwpml/2011/paragraph">
            <p:p paraPrIDRef="0"><p:run charPrIDRef="0">{runs}</p:run></p:p></hs:sec>"#
    );
    for (path, contents) in [
        ("mimetype", "application/hwp+zip"),
        (
            "Contents/content.hpf",
            r#"<opf:package xmlns:opf="http://www.idpf.org/2007/opf/">
                <opf:manifest><opf:item id="s" href="Contents/section0.xml"
                    media-type="application/xml"/></opf:manifest>
                <opf:spine><opf:itemref idref="s"/></opf:spine></opf:package>"#,
        ),
        (
            "Contents/header.xml",
            r#"<hh:head xmlns:hh="http://www.hancom.co.kr/hwpml/2011/head" secCnt="1">
                <hh:refList>
                  <hh:charProperties itemCnt="1"><hh:charPr id="0" height="1000"></hh:charPr></hh:charProperties>
                  <hh:paraProperties itemCnt="1"><hh:paraPr id="0"></hh:paraPr></hh:paraProperties>
                </hh:refList>
                <hh:trackChanges itemCnt="1"><hh:trackChange id="7" type="Delete" hide="1"/>
                </hh:trackChanges></hh:head>"#,
        ),
        ("Contents/section0.xml", &section),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// 접두사가 다른 HWPX에서 글맵시의 위치와 그룹 소속이 저장 왕복 후에도 유지되는지 확인한다.
#[test]
fn hwpx_text_art_keeps_slots_and_group_ownership_after_visibility_projection() {
    let bytes = hwpx_with_hidden_deletion(
        r#"<p:t>앞</p:t>
        <p:textart text="가&amp;&#xB098;&quot;&#10;끝"><p:sz width="500" height="300"/>
          <p:pos treatAsChar="1"/><p:textartPr fontName="A&amp;B" textShape="WAVE1"/>
        </p:textart>
        <p:t>중간</p:t>
        <p:deleteBegin Id="1" TcId="7"/><p:textart text="숨김"/><p:deleteEnd Id="1" TcId="7"/>
        <p:container><p:pos treatAsChar="1"/>
          <p:textart text="그룹"/><p:textart text="그룹 2"></p:textart>
        </p:container><p:t>뒤</p:t>"#,
    );
    let document = parse_hwpx(&bytes).unwrap();
    let saved = parse_hwpx(&serialize_hwpx(&document).unwrap()).unwrap();
    for doc in [&document, &saved] {
        let para = doc.sections[0]
            .paragraphs
            .iter()
            .find(|p| p.text == "앞중간뒤")
            .unwrap();
        let shapes: Vec<_> = para
            .controls
            .iter()
            .zip(para.control_text_positions())
            .filter_map(|(ctrl, pos)| match ctrl {
                Control::Shape(shape) => Some((shape.as_ref(), pos)),
                _ => None,
            })
            .collect();
        let [(ShapeObject::TextArt(art), 1), (ShapeObject::Group(group), 3)] = shapes.as_slice()
        else {
            panic!("문단에는 글맵시와 그룹의 슬롯만 있어야 한다");
        };
        assert_eq!(art.text, "가&나\"\n끝");
        assert_eq!(art.font_name, "A&B");
        assert_eq!(art.text_shape.as_deref(), Some("WAVE1"));
        assert_eq!((art.common.width, art.common.height), (500, 300));
        assert!(art.common.treat_as_char);
        let [ShapeObject::TextArt(first), ShapeObject::TextArt(second)] = group.children.as_slice()
        else {
            panic!("그룹이 글맵시 자식 둘을 소유해야 한다");
        };
        assert_eq!(first.text, "그룹");
        assert_eq!(second.text, "그룹 2");
    }
}

/// 개체 태그와 레벨, 페이로드를 지정한 합성 HWP 레코드를 반환한다.
fn record(tag_id: u16, level: u16, data: Vec<u8>) -> Record {
    Record {
        tag_id,
        level,
        size: data.len() as u32,
        data,
    }
}

/// 최상위 및 그룹 글맵시를 HWP 레코드에서 읽고, 잘린 문자열의 기본값을 확인한다.
#[test]
fn hwp_text_art_reads_own_detail_and_survives_truncation() {
    let mut detail: Vec<u8> = [0i32, 0, 500, 0, 500, 300, 0, 300]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    for value in ["글맵시😀", "테스트 글꼴"] {
        let units: Vec<_> = value.encode_utf16().collect();
        detail.extend((units.len() as u16).to_le_bytes());
        detail.extend(units.into_iter().flat_map(u16::to_le_bytes));
    }
    let tail = [1u32, 26, 120, 100, 0, 0, 0, 0, 0, 0];
    detail.extend(0u16.to_le_bytes());
    detail.extend(tail.into_iter().flat_map(u32::to_le_bytes));
    let component = record(
        tags::HWPTAG_SHAPE_COMPONENT,
        1,
        tags::SHAPE_TEXTART_ID.to_le_bytes().repeat(2),
    );
    for (data, expected) in [(&detail[..], "글맵시😀"), (&detail[..35], "")] {
        let ctrl = parse_control(
            tags::CTRL_GEN_SHAPE,
            &[],
            &[
                component.clone(),
                record(tags::HWPTAG_SHAPE_COMPONENT_TEXTART, 2, data.to_vec()),
            ],
        );
        let Control::Shape(shape) = &ctrl else {
            panic!("글맵시 컨트롤")
        };
        let ShapeObject::TextArt(art) = shape.as_ref() else {
            panic!("글맵시 도형")
        };
        assert_eq!(art.text, expected);
        assert_eq!(art.corners[2].x, 500);
        assert_eq!(
            art.font_name,
            if expected.is_empty() {
                ""
            } else {
                "테스트 글꼴"
            }
        );
        let mut records = Vec::new();
        serialize_control(&ctrl, 0, None, &mut records);
        if data.len() == detail.len() {
            let saved_detail = records
                .iter()
                .find(|record| record.tag_id == tags::HWPTAG_SHAPE_COMPONENT_TEXTART)
                .unwrap();
            assert_eq!(saved_detail.data, detail);
        }
        let ctrl = parse_control(tags::CTRL_GEN_SHAPE, &records[0].data[4..], &records[1..]);
        assert!(matches!(ctrl, Control::Shape(shape)
            if matches!(shape.as_ref(), ShapeObject::TextArt(art) if art.text == expected)));
    }

    let group = parse_control(
        tags::CTRL_GEN_SHAPE,
        &[],
        &[
            record(
                tags::HWPTAG_SHAPE_COMPONENT,
                1,
                tags::SHAPE_CONTAINER_ID.to_le_bytes().repeat(2),
            ),
            record(
                tags::HWPTAG_SHAPE_COMPONENT,
                2,
                tags::SHAPE_TEXTART_ID.to_le_bytes().to_vec(),
            ),
            record(tags::HWPTAG_SHAPE_COMPONENT_TEXTART, 3, detail),
        ],
    );
    let mut records = Vec::new();
    serialize_control(&group, 0, None, &mut records);
    let saved = parse_control(tags::CTRL_GEN_SHAPE, &records[0].data[4..], &records[1..]);
    for ctrl in [group, saved] {
        let Control::Shape(shape) = ctrl else {
            panic!("그룹 컨트롤")
        };
        let ShapeObject::Group(group) = shape.as_ref() else {
            panic!("그룹 도형")
        };
        assert!(matches!(group.children.as_slice(),
            [ShapeObject::TextArt(art)] if art.text == "글맵시😀"));
    }
}
