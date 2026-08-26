import { describe, expect, it } from "vitest";
import {
  formatImageAttachmentAnnotationsForSubmission,
  hasImageAttachmentAnnotations,
  fitAspectRatioWithinBox,
  imagePointFromClientPosition,
  stripImageAttachmentAnnotationsForSubmission,
} from "./imageAttachmentAnnotations";

describe("image attachment annotations", () => {
  it("calculates stable percentages from the displayed image bounds", () => {
    expect(
      imagePointFromClientPosition(
        { left: 10, top: 20, width: 100, height: 200 },
        41.86,
        117.8,
      ),
    ).toEqual({ xPercent: 31.9, yPercent: 48.9 });

    expect(
      imagePointFromClientPosition(
        { left: 10, top: 20, width: 200, height: 400 },
        73.72,
        215.6,
      ),
    ).toEqual({ xPercent: 31.9, yPercent: 48.9 });
  });

  it("clamps clicks to the image bounds", () => {
    expect(
      imagePointFromClientPosition(
        { left: 10, top: 20, width: 100, height: 200 },
        0,
        240,
      ),
    ).toEqual({ xPercent: 0, yPercent: 100 });
  });

  it("fits a selected preview ratio inside the available panel", () => {
    expect(fitAspectRatioWithinBox(800, 600, 16 / 9)).toEqual({
      width: 800,
      height: 450,
    });
    expect(fitAspectRatioWithinBox(400, 600, 1)).toEqual({
      width: 400,
      height: 400,
    });
  });

  it("formats annotated images by their image-only attachment order", () => {
    const attachments = [
      {
        id: "notes",
        fileName: "notes.txt",
        filePath: "C:/tmp/notes.txt",
        sizeBytes: 10,
        mimeType: "text/plain",
      },
      {
        id: "image-one",
        fileName: "first.png",
        filePath: "C:/tmp/first.png",
        sizeBytes: 20,
        mimeType: "image/png",
      },
      {
        id: "image-two",
        fileName: "second.png",
        filePath: "C:/tmp/second.png",
        sizeBytes: 30,
        mimeType: "image/png",
        imageAnnotations: [
          {
            id: "point-1",
            number: 1,
            xPercent: 31.9,
            yPercent: 48.9,
            comment: "图片显示区域",
          },
          {
            id: "point-2",
            number: 2,
            xPercent: 76.2,
            yPercent: 10,
            comment: "图片放大比例",
          },
        ],
      },
    ];

    expect(formatImageAttachmentAnnotationsForSubmission("请处理这些位置", attachments)).toBe(
      "请处理这些位置\n\n" +
        "图像 2:\n" +
        "1.(x:31.9%,y:48.9%)图片显示区域\n" +
        "2.(x:76.2%,y:10%)图片放大比例",
    );
    expect(hasImageAttachmentAnnotations(attachments)).toBe(true);
  });

  it("removes draft-only annotation state from transport attachments", () => {
    const attachments = [
      {
        id: "image-one",
        fileName: "first.png",
        filePath: "C:/tmp/first.png",
        sizeBytes: 20,
        mimeType: "image/png",
        imageAnnotations: [
          {
            id: "point-1",
            number: 1,
            xPercent: 20,
            yPercent: 30,
            comment: "按钮",
          },
        ],
      },
    ];

    expect(stripImageAttachmentAnnotationsForSubmission(attachments)).toEqual([
      {
        id: "image-one",
        fileName: "first.png",
        filePath: "C:/tmp/first.png",
        sizeBytes: 20,
        mimeType: "image/png",
      },
    ]);
    expect(attachments[0].imageAnnotations).toHaveLength(1);
  });

  it("keeps ordinary input unchanged when no image annotation is pending", () => {
    const attachments = [
      {
        id: "image-one",
        fileName: "first.png",
        filePath: "C:/tmp/first.png",
        sizeBytes: 20,
        mimeType: "image/png",
      },
    ];

    expect(formatImageAttachmentAnnotationsForSubmission("  普通消息  ", attachments)).toBe(
      "普通消息",
    );
    expect(hasImageAttachmentAnnotations(attachments)).toBe(false);
  });
});
