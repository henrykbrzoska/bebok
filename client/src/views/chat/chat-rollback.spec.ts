import { Message } from '../../core/engine.dtos';
import { retryDraft } from './chat';

describe('retryDraft', () => {
  it('restores the selected user text and attachments for editing in a retry branch', () => {
    const message: Message = {
      id: 'user-1',
      role: 'user',
      parts: [
        { type: 'image', media_type: 'image/png', data: 'aGVsbG8=', name: 'diagram.png', bytes: 5 },
        { type: 'text', text: 'Please revise this.' },
      ],
    };

    const draft = retryDraft(message);

    expect(draft.text).toBe('Please revise this.');
    expect(draft.attachments).toEqual([
      jasmine.objectContaining({
        media_type: 'image/png',
        base64: 'aGVsbG8=',
        dataUrl: 'data:image/png;base64,aGVsbG8=',
        name: 'diagram.png',
        size: 5,
      }),
    ]);
  });

  it('keeps an image-only prompt retryable', () => {
    const draft = retryDraft({
      id: 'user-image-only',
      role: 'user',
      parts: [{ type: 'image', media_type: 'image/jpeg', data: 'YWJj' }],
    });

    expect(draft.text).toBe('');
    expect(draft.attachments.length).toBe(1);
    expect(draft.attachments[0].size).toBe(3);
  });
});
